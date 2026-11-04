//! Strip Silence: the sound in audio clips found by level
//! ([`faderframe_analysis::silence`]) and the silence between taken out
//! (or the clips split there, or the sound put on a track of its own as
//! numbered clips — dialogue lines, drum hits), one undo step.
//!
//! Opening the dialog names the clips; their sources' levels are measured
//! once on a helper thread (every channel's peak per `silence::HOP`
//! frames) and kept, so each turn of a setting finds the regions again at
//! once — the arranger paints them on the clips while the dialog is open
//! ([`Session::strip_silence_preview`]). Warped and reversed clips are
//! looked at as they play: a clip's levels are read through its time map.

use crate::{Result, Session, SessionError, UiRequest};
use faderframe_analysis::silence::{self, HOP, Levels};
pub use faderframe_analysis::silence::{SilenceSettings, Span};
use faderframe_core::{AudioSourceId, ClipId, TrackId};
use faderframe_engine::Source;
use faderframe_project::{AudioClip, Clip, ClipContent, Command, Track, TrackKind};
use faderframe_timeline::MusicalTime;
use std::collections::HashMap;
use std::sync::Arc;
use std::thread::JoinHandle;

/// What Strip Silence does with the regions it finds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StripMode {
    /// The silence goes; each region of sound is a clip of its own.
    #[default]
    Strip,
    /// The clips are split at the regions' edges; nothing goes.
    Separate,
    /// The regions as numbered clips on a new track under the clips'
    /// own, which are muted (dialogue lines, drum hits).
    NewClips,
}

impl StripMode {
    pub const ALL: [StripMode; 3] = [StripMode::Strip, StripMode::Separate, StripMode::NewClips];

    pub fn label(self) -> &'static str {
        match self {
            StripMode::Strip => "Strip",
            StripMode::Separate => "Separate",
            StripMode::NewClips => "Strip to New Clips",
        }
    }

    fn step(self) -> &'static str {
        match self {
            StripMode::Strip => "Strip Silence",
            StripMode::Separate => "Separate at Silence",
            StripMode::NewClips => "Strip Silence to New Clips",
        }
    }
}

/// Strip Silence from the views.
#[derive(Clone, Debug, PartialEq)]
pub enum StripSilenceOp {
    /// Open the dialog on these clips (empty: the edit range's audio on
    /// the selected tracks when there is a range — only silence inside it
    /// goes —, else the selected audio clips).
    Open { clips: Vec<ClipId> },
    /// New settings (the preview follows).
    Settings(SilenceSettings),
    /// Do it to the clips (one undo step); the dialog stays open on what
    /// is left of them.
    Apply(StripMode),
    /// The dialog closed: no more preview.
    Close,
}

/// What the dialog shows: the clips and, per clip, the regions found
/// (clip-relative project frames). Clips whose levels are still measured
/// have none yet.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StripPreview {
    pub clips: Vec<ClipId>,
    /// Only silence inside this range goes (opened on an edit range).
    pub range: Option<(MusicalTime, MusicalTime)>,
    pub regions: HashMap<ClipId, Vec<Span>>,
    pub measuring: bool,
}

impl StripPreview {
    /// Regions found in all, and the silent frames between them.
    pub fn counts(&self) -> (usize, usize) {
        let regions = self.regions.values().map(Vec::len).sum();
        (regions, self.clips.len())
    }
}

/// A source's levels and their rate (`None`: it could not be read).
type Measured = Option<(Levels, f64)>;

/// Strip Silence's state in the session.
#[derive(Default)]
pub(crate) struct StripSilenceState {
    settings: SilenceSettings,
    open: bool,
    preview: StripPreview,
    /// Measured sources (levels at the source's own rate, that rate).
    levels: HashMap<AudioSourceId, (Arc<Levels>, f64)>,
    jobs: Vec<(AudioSourceId, JoinHandle<Measured>)>,
}

/// Levels of every channel of a source (helper thread).
fn measure(source: &Source) -> Measured {
    match source {
        Source::Memory(d) => {
            let views: Vec<&[f32]> = (0..d.num_channels()).map(|c| d.channel(c)).collect();
            Some((Levels::measure(&views), f64::from(d.sample_rate())))
        }
        Source::Stream(st) => {
            let f = faderframe_audio_files::wavstream::WavFile::open(st.path()).ok()?;
            let chunk = HOP * 1024;
            let mut bufs = vec![vec![0.0f32; chunk]; f.channels()];
            let mut scratch = Vec::new();
            let mut levels = Levels::default();
            let mut pos = 0u64;
            while pos < f.frames() {
                let n = chunk.min((f.frames() - pos) as usize);
                {
                    let mut slices: Vec<&mut [f32]> =
                        bufs.iter_mut().map(|c| &mut c[..n]).collect();
                    f.read(pos, &mut slices, &mut scratch).ok()?;
                }
                let views: Vec<&[f32]> = bufs.iter().map(|c| &c[..n]).collect();
                levels.extend(&views, n);
                pos += n as u64;
            }
            Some((levels, f64::from(f.sample_rate())))
        }
    }
}

/// The levels of what a clip plays, in clip frames at `project_rate`:
/// each hop of the clip reads the source's peaks over the source frames
/// it plays (through the warp map, backwards when reversed).
pub fn clip_levels(a: &AudioClip, levels: &Levels, rate: f64, project_rate: f64) -> Levels {
    let k = rate / project_rate.max(1.0);
    let span = a.source_span();
    let source_of = |out: i64| -> f64 {
        if a.reversed {
            (a.source_offset + span) as f64 - out as f64
        } else {
            a.source_at(out as f64)
        }
    };
    let hops = (a.length.max(0) as usize).div_ceil(HOP);
    let peaks = (0..hops)
        .map(|h| {
            let (u0, u1) = ((h * HOP) as i64, (((h + 1) * HOP) as i64).min(a.length));
            let (s0, s1) = (source_of(u0) * k, source_of(u1) * k);
            let (lo, hi) = (s0.min(s1), s0.max(s1));
            let a = (lo / HOP as f64).floor().max(0.0) as usize;
            let b = ((hi / HOP as f64).ceil() as usize).max(a + 1);
            levels
                .peaks
                .get(a.min(levels.peaks.len())..b.min(levels.peaks.len()))
                .map_or(0.0, |p| p.iter().fold(0.0f32, |m, v| m.max(*v)))
        })
        .collect();
    Levels {
        peaks,
        frames: a.length.max(0),
    }
}

/// `found` inside `[a, b)`, and everything outside it, as regions of a
/// clip `len` frames long (sorted, joined where they touch).
pub fn within_range(found: &[Span], a: i64, b: i64, len: i64) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    let mut push = |sp: Span| {
        if sp.is_empty() {
            return;
        }
        match out.last_mut() {
            Some(last) if sp.start <= last.end => last.end = last.end.max(sp.end),
            _ => out.push(sp),
        }
    };
    push(Span { start: 0, end: a });
    for f in found {
        push(Span {
            start: f.start.max(a),
            end: f.end.min(b),
        });
    }
    push(Span { start: b, end: len });
    out
}

impl Session {
    /// Strip Silence's settings (the dialog's, kept while the session
    /// lasts).
    pub fn strip_silence_settings(&self) -> SilenceSettings {
        self.strip_silence.settings
    }

    /// The regions the dialog would keep, while it is open.
    pub fn strip_silence_preview(&self) -> Option<&StripPreview> {
        self.strip_silence
            .open
            .then_some(&self.strip_silence.preview)
    }

    pub(crate) fn strip_silence(&mut self, op: StripSilenceOp) -> Result<()> {
        match op {
            StripSilenceOp::Open { clips } => {
                let range = self
                    .selection
                    .range
                    .filter(|r| clips.is_empty() && r.start < r.end)
                    .map(|r| (r.start, r.end));
                let clips = match range {
                    Some((a, b)) => {
                        let (tl, rate) = (&self.project.timeline, self.project.sample_rate);
                        self.selection
                            .tracks
                            .iter()
                            .flat_map(|t| self.project.clips_of(*t))
                            .filter(|c| c.start < b && c.end(tl, rate) > a)
                            .map(|c| c.id)
                            .collect()
                    }
                    None if clips.is_empty() => self.selected_arranger_clips(),
                    None => clips,
                };
                let clips: Vec<ClipId> = clips
                    .into_iter()
                    .filter(|c| {
                        self.project
                            .clip(*c)
                            .is_some_and(|c| matches!(c.content, ClipContent::Audio(_)))
                    })
                    .collect();
                if clips.is_empty() {
                    return Err(SessionError::Other(
                        "Strip Silence works on audio clips: select some".into(),
                    ));
                }
                self.strip_silence.open = true;
                self.strip_silence.preview.clips = clips;
                self.strip_silence.preview.range = range;
                self.measure_strip_sources();
                self.update_strip_preview();
                self.ui_requests.push(UiRequest::StripSilence);
            }
            StripSilenceOp::Settings(s) => {
                self.strip_silence.settings = s.clamped();
                self.update_strip_preview();
            }
            StripSilenceOp::Apply(mode) => self.apply_strip_silence(mode)?,
            StripSilenceOp::Close => {
                self.strip_silence.open = false;
                self.strip_silence.preview = StripPreview::default();
            }
        }
        self.revision += 1;
        Ok(())
    }

    /// Start measuring the sources of the dialog's clips not measured yet.
    fn measure_strip_sources(&mut self) {
        let wanted: Vec<AudioSourceId> = self
            .strip_silence
            .preview
            .clips
            .iter()
            .filter_map(|c| match &self.project.clip(*c)?.content {
                ClipContent::Audio(a) => Some(a.source),
                _ => None,
            })
            .collect();
        for id in wanted {
            let st = &self.strip_silence;
            if st.levels.contains_key(&id) || st.jobs.iter().any(|(s, _)| *s == id) {
                continue;
            }
            let Some(source) = self.sources.get(&id).cloned() else {
                continue;
            };
            let job = std::thread::Builder::new()
                .name("faderframe-strip-silence".into())
                .spawn(move || measure(&source));
            match job {
                Ok(job) => self.strip_silence.jobs.push((id, job)),
                Err(e) => self.notify(
                    crate::NoticeLevel::Warning,
                    format!("Strip Silence could not measure a clip: {e}"),
                ),
            }
        }
    }

    /// Take finished measurements (the tick).
    pub(crate) fn poll_strip_silence(&mut self) {
        let mut got = false;
        let mut i = 0;
        while i < self.strip_silence.jobs.len() {
            if self.strip_silence.jobs[i].1.is_finished() {
                let (id, job) = self.strip_silence.jobs.swap_remove(i);
                if let Ok(Some((levels, rate))) = job.join() {
                    self.strip_silence
                        .levels
                        .insert(id, (Arc::new(levels), rate));
                }
                got = true;
            } else {
                i += 1;
            }
        }
        if got {
            self.update_strip_preview();
            self.revision += 1;
        }
    }

    /// Wait (tests, dev actions) until the dialog's clips are measured.
    pub fn wait_for_strip_silence(&mut self, timeout: std::time::Duration) -> bool {
        let until = std::time::Instant::now() + timeout;
        loop {
            self.poll_strip_silence();
            if self.strip_silence.jobs.is_empty() {
                return true;
            }
            if std::time::Instant::now() > until {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Regions of one clip (clip frames), `None` while unmeasured.
    fn strip_regions(&self, c: &Clip) -> Option<Vec<Span>> {
        let ClipContent::Audio(a) = &c.content else {
            return None;
        };
        let (levels, rate) = self.strip_silence.levels.get(&a.source)?;
        let project_rate = f64::from(self.project.sample_rate);
        let clip = clip_levels(a, levels, *rate, project_rate);
        let found = silence::regions(&clip, project_rate, &self.strip_silence.settings);
        let Some((ra, rb)) = self.strip_silence.preview.range else {
            return Some(found);
        };
        // Opened on a range: what lies outside it stays as sound.
        let tl = &self.project.timeline;
        let base = tl.to_samples(c.start, project_rate);
        let frame = |t| (tl.to_samples(t, project_rate) - base).clamp(0, a.length);
        Some(within_range(&found, frame(ra), frame(rb), a.length))
    }

    fn update_strip_preview(&mut self) {
        let clips = std::mem::take(&mut self.strip_silence.preview.clips);
        let mut regions = HashMap::new();
        for id in clips.iter().copied() {
            if let Some(r) = self.project.clip(id).and_then(|c| self.strip_regions(c)) {
                regions.insert(id, r);
            }
        }
        let measuring = !self.strip_silence.jobs.is_empty();
        let range = self.strip_silence.preview.range;
        self.strip_silence.preview = StripPreview {
            clips,
            range,
            regions,
            measuring,
        };
    }

    /// The edit, one step; the dialog then looks at what is left.
    fn apply_strip_silence(&mut self, mode: StripMode) -> Result<()> {
        if !self.strip_silence.jobs.is_empty() {
            return Err(SessionError::Other(
                "Strip Silence is still measuring the clips".into(),
            ));
        }
        self.update_strip_preview();
        let preview = self.strip_silence.preview.clone();
        let rate = self.project.sample_rate;
        let tl = self.project.timeline.clone();
        let mut commands = Vec::new();
        let mut kept: Vec<ClipId> = Vec::new();
        // New tracks (`NewClips`): one under each source track.
        let mut new_tracks: HashMap<TrackId, TrackId> = HashMap::new();
        let mut numbers: HashMap<TrackId, usize> = HashMap::new();
        let mut clips: Vec<Clip> = preview
            .clips
            .iter()
            .filter_map(|c| self.project.clip(*c).cloned())
            .collect();
        clips.sort_by_key(|c| (c.track, c.start));
        for c in &clips {
            let Some(regions) = preview.regions.get(&c.id) else {
                continue;
            };
            let ClipContent::Audio(a) = &c.content else {
                continue;
            };
            let base = tl.to_samples(c.start, f64::from(rate));
            let time = |frame: i64| tl.at_sample(base + frame, f64::from(rate));
            let whole = regions.len() == 1 && regions[0].start == 0 && regions[0].end >= a.length;
            match mode {
                StripMode::Separate => {
                    let mut cuts: Vec<i64> = regions
                        .iter()
                        .flat_map(|r| [r.start, r.end])
                        .filter(|&f| f > 0 && f < a.length)
                        .collect();
                    cuts.sort_unstable();
                    cuts.dedup();
                    kept.push(c.id);
                    // Right to left, so the clip being split stays `c`.
                    for f in cuts.into_iter().rev() {
                        let new_clip: ClipId = self.project.ids.allocate();
                        commands.push(Command::SplitClip {
                            clip: c.id,
                            at: time(f),
                            new_clip,
                        });
                        kept.push(new_clip);
                    }
                }
                StripMode::Strip if whole => kept.push(c.id),
                StripMode::Strip => {
                    commands.push(Command::RemoveClip { clip: c.id });
                    for r in regions {
                        let id: ClipId = self.project.ids.allocate();
                        if let Some(piece) = c.slice(time(r.start), time(r.end), id, &tl, rate) {
                            commands.push(Command::AddClip {
                                clip: Box::new(piece),
                            });
                            kept.push(id);
                        }
                    }
                }
                StripMode::NewClips => {
                    let track = match new_tracks.get(&c.track) {
                        Some(t) => *t,
                        None => {
                            let t = self.hits_track(c.track, &mut commands)?;
                            new_tracks.insert(c.track, t);
                            t
                        }
                    };
                    let n = numbers.entry(c.track).or_insert(0);
                    for r in regions {
                        let id: ClipId = self.project.ids.allocate();
                        if let Some(mut piece) = c.slice(time(r.start), time(r.end), id, &tl, rate)
                        {
                            *n += 1;
                            piece.track = track;
                            piece.muted = false;
                            piece.name = format!("{} {:02}", c.name, n);
                            commands.push(Command::AddClip {
                                clip: Box::new(piece),
                            });
                            kept.push(id);
                        }
                    }
                    if !c.muted {
                        commands.push(Command::SetClipMuted {
                            clip: c.id,
                            muted: true,
                        });
                    }
                }
            }
        }
        if commands.is_empty() {
            return Ok(());
        }
        self.batch(mode.step(), commands)?;
        kept.retain(|c| self.project.clip(*c).is_some());
        if !kept.is_empty() {
            self.dispatch(crate::Action::SelectClips {
                clips: kept.clone(),
                mode: crate::SelectMode::Replace,
            })?;
        }
        self.strip_silence.preview.clips = kept;
        self.update_strip_preview();
        Ok(())
    }

    /// A new audio track right under `of` for its regions (commands
    /// added to `commands`), with its layout, output and folder.
    fn hits_track(&mut self, of: TrackId, commands: &mut Vec<Command>) -> Result<TrackId> {
        let Some(src) = self.project.track(of).cloned() else {
            return Err(SessionError::Other(format!("no track {of}")));
        };
        let index = self
            .project
            .tracks
            .iter()
            .position(|t| t.id == of)
            .map_or(self.project.tracks.len(), |i| i + 1);
        let id: TrackId = self.project.ids.allocate();
        let mut t = Track::new(
            id,
            TrackKind::Audio,
            format!("{} Clips", src.name),
            src.color,
        )
        .with_layout(src.layout);
        t.folder = src.folder;
        t.output = src.output;
        commands.push(Command::AddTrack {
            track: Box::new(t),
            index,
        });
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_project::{ClipFades, StretchSettings, Warp};

    fn clip(length: i64) -> AudioClip {
        AudioClip {
            source: AudioSourceId(1),
            source_offset: 1_000,
            length,
            gain_db: 0.0,
            fades: ClipFades::default(),
            stretch: StretchSettings::Off,
            reversed: false,
            warp: None,
            pitch: None,
            effects: None,
            spectral: None,
            envelope: None,
        }
    }

    #[test]
    fn a_range_keeps_what_lies_outside_it() {
        let sp = |start, end| Span { start, end };
        let found = [sp(100, 200), sp(400, 500), sp(800, 900)];
        assert_eq!(
            within_range(&found, 300, 700, 1_000),
            vec![sp(0, 300), sp(400, 500), sp(700, 1_000)]
        );
        // A region across an edge joins the outside.
        assert_eq!(
            within_range(&found, 150, 1_000, 1_000),
            vec![sp(0, 200), sp(400, 500), sp(800, 900)]
        );
    }

    #[test]
    fn a_clip_reads_its_part_of_the_source_as_it_plays() {
        // Sound in source hops 20..30 (frames 1 280..1 920).
        let mut levels = Levels {
            peaks: vec![0.0; 100],
            frames: 6_400,
        };
        for p in &mut levels.peaks[20..30] {
            *p = 0.5;
        }
        let a = clip(4_000);
        let l = clip_levels(&a, &levels, 48_000.0, 48_000.0);
        let loud: Vec<usize> = (0..l.peaks.len()).filter(|&i| l.peaks[i] > 0.0).collect();
        // Clip frame 0 = source frame 1 000: the sound from frame 280.
        assert_eq!(loud.first(), Some(&4));
        assert_eq!(loud.last(), Some(&14));
        // Reversed: the sound comes near the end instead.
        let r = AudioClip {
            reversed: true,
            ..clip(4_000)
        };
        let l = clip_levels(&r, &levels, 48_000.0, 48_000.0);
        let first = (0..l.peaks.len()).find(|&i| l.peaks[i] > 0.0);
        assert!(first.is_some_and(|i| i > 40), "{first:?}");
        // Played twice as slow through a warp: twice as long.
        let w = AudioClip {
            warp: Some(Warp::uniform(4_000).scaled(4_000, 8_000)),
            length: 8_000,
            ..clip(8_000)
        };
        let l = clip_levels(&w, &levels, 48_000.0, 48_000.0);
        let n = l.peaks.iter().filter(|p| **p > 0.0).count();
        assert!((19..=23).contains(&n), "{n}");
    }
}
