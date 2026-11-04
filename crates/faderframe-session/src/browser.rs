//! The media browser: sound files and MIDI files found in places (the
//! project's media, its Samples folder, folders the user adds, recent
//! files), searched and filtered by what they are (type, length, tempo,
//! key, tags, rating), heard before they are used, and dragged into the
//! arranger, the clip launcher or a sampler.
//!
//! * **The index** ([`index`]): what is known of every file seen — its
//!   container's facts, then its analysis (tempo and key by
//!   `faderframe_analysis::{tempo, chroma}`, a waveform thumbnail, a
//!   [`SoundPrint`] for Find Similar) — kept in `<data>/browser/index.json`
//!   for as long as the file's size and modification time are those it was
//!   made from. The user's tags, ratings, recent files and folders are a
//!   file of their own beside it (`user.json`). A worker ([`scan`]) lists
//!   folders and reads/analyses the files in view, below the UI's priority.
//! * **Previews** play through the engine's monitor path beside the mix
//!   ([`faderframe_engine::nodes::file_preview`]): decoded on a worker,
//!   stretched to the project's tempo (a loop starts on the next bar while
//!   the transport plays, in step with it) and transposed to its key when
//!   asked, at their own volume; MIDI files play the selected instrument
//!   ([`faderframe_engine::midi_preview`]).
//! * **Placing**: files dropped on the arranger or a launcher slot are
//!   imported as any file is, with the same tempo and key match applied as
//!   a warp (a launcher clip: Follow Tempo) and a transposition of the clip
//!   — one undo step. Nothing here edits the project otherwise.

pub mod index;
mod preview;
pub mod scan;
pub(crate) mod smf;

use crate::media::{ImportJob, ImportTarget};
use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_analysis::sound_print::{self, SoundPrint};
use faderframe_core::{SceneId, TrackId};
use faderframe_engine::midi_preview::{MidiPreview, MidiPreviewStatus};
use faderframe_engine::nodes::{PreviewAudio, PreviewSound, PreviewStart};
use faderframe_project::harmony::{Key, Scale};
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, Command, SourceSpec, StretchSettings, Track,
    TrackColor, TrackKind, Warp, WarpAlgorithm,
};
use faderframe_timeline::MusicalTime;
pub use index::{Analysis, Facts, MAX_RATING};
use index::{Entry, FileKind, Index, UserData};
use preview::{Change, PreviewJob};
use scan::{Found, Listing, Scanner};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// What a browser drag carries to other views: `browser:` and a path a
/// line.
pub const PAYLOAD: &str = "browser:";
/// Most rows Find Similar lists.
pub const MAX_SIMILAR: usize = 100;
/// Most files of a listing read and analysed by themselves (an explicit
/// "Analyse Everything Here" takes the rest).
pub const MAX_QUEUED: usize = 1_000;
/// How long after a change the index is written.
const SAVE_AFTER: Duration = Duration::from_secs(3);
/// A loop starting on the next bar starts at least this far ahead of
/// what is processed (seconds).
const START_LEAD: f64 = 0.05;
/// The tempo filter's reach around the project's tempo.
const TEMPO_REACH: f64 = 0.06;

/// A drag's payload for `paths`.
pub fn payload(paths: &[PathBuf]) -> String {
    let mut s = String::from(PAYLOAD);
    for p in paths {
        s.push('\n');
        s.push_str(&p.to_string_lossy());
    }
    s
}

/// The files a browser payload carries (`None`: not one).
pub fn parse_payload(text: &str) -> Option<Vec<PathBuf>> {
    let rest = text.strip_prefix(PAYLOAD)?;
    let files: Vec<PathBuf> = rest
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect();
    (!files.is_empty()).then_some(files)
}

/// Where the browser looks.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Place {
    /// The project's media folder.
    ProjectMedia,
    /// The project's Samples folder (samplers' samples).
    Samples,
    /// Files previewed or placed lately.
    Recent,
    /// The user's home folder.
    Home,
    /// A folder the user added.
    Folder(PathBuf),
}

impl Place {
    pub fn label(&self) -> String {
        match self {
            Place::ProjectMedia => "Project Media".into(),
            Place::Samples => "Samples".into(),
            Place::Recent => "Recent".into(),
            Place::Home => "Home".into(),
            Place::Folder(p) => name_of(p),
        }
    }
}

fn name_of(p: &Path) -> String {
    p.file_name().map_or_else(
        || p.to_string_lossy().to_string(),
        |n| n.to_string_lossy().to_string(),
    )
}

fn home() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

/// How previews play (and how placed files are matched).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PreviewSettings {
    /// In the project's tempo (loops stretched; on the next bar while the
    /// transport plays).
    pub sync: bool,
    /// In the project's key (transposed).
    pub key: bool,
    /// Loops and MIDI files go round until stopped.
    pub looped: bool,
    /// A file previews when it is selected.
    pub auto: bool,
    /// The preview's volume (dB).
    pub volume_db: f32,
}

impl Default for PreviewSettings {
    fn default() -> Self {
        Self {
            sync: true,
            key: false,
            looped: true,
            auto: true,
            volume_db: -6.0,
        }
    }
}

/// Which files are shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum KindFilter {
    #[default]
    All,
    Audio,
    Midi,
    /// Audio in time: a clear tempo, an even number of beats.
    Loops,
    /// Short audio that is not a loop.
    OneShots,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LengthFilter {
    #[default]
    Any,
    /// Up to 2 s.
    Short,
    /// 2 to 15 s.
    Medium,
    /// Longer than 15 s.
    Long,
}

impl LengthFilter {
    pub const ALL: [LengthFilter; 4] = [Self::Any, Self::Short, Self::Medium, Self::Long];

    pub fn label(self) -> &'static str {
        match self {
            Self::Any => "Any Length",
            Self::Short => "Up to 2 s",
            Self::Medium => "2–15 s",
            Self::Long => "Longer than 15 s",
        }
    }

    fn admits(self, seconds: Option<f64>) -> bool {
        match (self, seconds) {
            (Self::Any, _) => true,
            (_, None) => false,
            (Self::Short, Some(s)) => s <= 2.0,
            (Self::Medium, Some(s)) => s > 2.0 && s <= 15.0,
            (Self::Long, Some(s)) => s > 15.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum KeyFilter {
    #[default]
    Any,
    /// Keys that sit with the project's (the same notes, or a fifth away).
    Project,
    Key(Key),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum TempoFilter {
    #[default]
    Any,
    /// Within 6 % of the project's tempo (or half or double it).
    Project,
    /// From … to (BPM).
    Range(f64, f64),
}

/// The filters (all must pass).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BrowserFilter {
    pub kind: KindFilter,
    pub length: LengthFilter,
    pub key: KeyFilter,
    pub tempo: TempoFilter,
    pub tag: Option<String>,
    /// At least this many stars (0: any).
    pub rating: u8,
}

impl BrowserFilter {
    pub fn is_any(&self) -> bool {
        *self == Self::default()
    }
}

/// What the list is sorted by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortBy {
    #[default]
    Name,
    Kind,
    Length,
    Tempo,
    Key,
    Rating,
}

/// What a row is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowItem {
    /// The folder above.
    Up,
    Folder,
    Audio,
    Midi,
}

/// One line of the list.
#[derive(Clone, Debug, PartialEq)]
pub struct BrowserRow {
    pub path: PathBuf,
    pub name: String,
    pub item: RowItem,
    /// "WAV 24-bit", "MP3", "MIDI 1" (empty: not read yet).
    pub format: String,
    pub seconds: Option<f64>,
    pub channels: u16,
    pub rate: u32,
    pub bpm: Option<f64>,
    pub key: Option<Key>,
    pub rating: u8,
    pub tags: Vec<String>,
    /// The waveform (or notes) thumbnail, hex levels.
    pub thumb: String,
    /// Audio in time.
    pub is_loop: bool,
    /// Not analysed yet.
    pub pending: bool,
    /// Find Similar: how like the sound it is (0…1).
    pub likeness: Option<f32>,
}

impl BrowserRow {
    pub fn thumb_levels(&self) -> Vec<f32> {
        index::thumb_levels(&self.thumb)
    }

    pub fn is_file(&self) -> bool {
        matches!(self.item, RowItem::Audio | RowItem::Midi)
    }
}

/// What a file would be changed by to match the project.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fit {
    /// The file's tempo, doubled or halved to the nearest of the
    /// project's (`None`: not matched).
    pub bpm: Option<f64>,
    /// The project's tempo it plays at.
    pub project_bpm: f64,
    /// Transposition (semitones; 0 none).
    pub semitones: i32,
}

impl Fit {
    /// Input frames per output frame.
    pub fn speed(&self) -> f64 {
        self.bpm.map_or(1.0, |b| self.project_bpm / b)
    }

    pub fn text(&self) -> String {
        let mut parts = Vec::new();
        if let Some(b) = self.bpm
            && (b - self.project_bpm).abs() > 0.01
        {
            parts.push(format!("{b:.1} → {:.1} BPM", self.project_bpm));
        }
        if self.semitones != 0 {
            parts.push(format!("{:+} st", self.semitones));
        }
        parts.join(", ")
    }
}

/// The preview playing.
#[derive(Clone, Debug, PartialEq)]
pub struct PreviewInfo {
    pub path: PathBuf,
    pub name: String,
    /// Where it is (0…1 of its length; `None`: waiting to start).
    pub position: Option<f64>,
    /// "120 → 96 BPM, +2 st" (empty: as it is).
    pub fit: String,
    pub midi: bool,
    /// The audio made for it, in frames at the stream's rate (once all is
    /// made; MIDI: its length).
    pub frames: Option<i64>,
    /// It loops this many frames (0: plays once, or the audio's length).
    pub loop_frames: i64,
    /// It waits for a bar (in step with the transport).
    pub synced: bool,
}

/// What the browser does (none of it is undoable except placing files).
#[derive(Clone, Debug, PartialEq)]
pub enum BrowserOp {
    /// Look in a place (its top folder).
    Open(Place),
    /// Look in a folder (inside the place).
    Enter(PathBuf),
    /// The folder above.
    Up,
    /// List again (files added on disk).
    Refresh,
    Search(String),
    Filter(BrowserFilter),
    /// Sort by this (again: the other way round).
    Sort(SortBy),
    /// Search the folders inside too.
    Deep(bool),
    AddFolder(PathBuf),
    RemoveFolder(PathBuf),
    Select(Vec<PathBuf>),
    /// Hear a file (MIDI: through the selected instrument).
    Preview(PathBuf),
    StopPreview,
    Settings(PreviewSettings),
    /// Stars (0 none … 5).
    Rate {
        paths: Vec<PathBuf>,
        stars: u8,
    },
    Tag {
        paths: Vec<PathBuf>,
        tag: String,
        on: bool,
    },
    /// Ask for a new tag's name for these files
    /// ([`crate::UiRequest::NewBrowserTag`]).
    AskTag(Vec<PathBuf>),
    /// List the sounds most like this one.
    FindSimilar(PathBuf),
    ClearSimilar,
    /// Read and analyse these again (or every file here and below: empty).
    Analyse(Vec<PathBuf>),
    /// Files onto the arranger (on `track`, else new tracks, at `at`),
    /// matched to the project as previews are. One undo step.
    Place {
        files: Vec<PathBuf>,
        track: Option<TrackId>,
        at: MusicalTime,
    },
    /// A file into a launcher slot (and the next ones down).
    ToLauncher {
        files: Vec<PathBuf>,
        track: TrackId,
        scene: SceneId,
    },
}

impl BrowserOp {
    /// Does it edit the project?
    pub fn edits(&self) -> bool {
        matches!(self, Self::Place { .. } | Self::ToLauncher { .. })
    }
}

/// The preview being heard.
struct Previewing {
    path: PathBuf,
    fit: Option<Fit>,
    sound: Sound,
    /// Started on a bar, in step with the transport.
    synced: bool,
}

enum Sound {
    Audio {
        generation: u64,
        audio: Arc<PreviewAudio>,
        job: Option<PreviewJob>,
        /// A loop's length in frames (0: the audio's).
        loop_frames: i64,
    },
    Midi {
        status: Arc<MidiPreviewStatus>,
        length: i64,
    },
}

/// Files being imported to be placed.
struct Placing {
    job: ImportJob,
    fits: HashMap<PathBuf, Fit>,
    rhythmic: HashMap<PathBuf, bool>,
    to: Destination,
}

#[derive(Clone, Copy)]
enum Destination {
    Arranger,
    /// From this scene down, on the track asked for.
    Launcher {
        scene: SceneId,
    },
}

/// The browser's state in the session.
pub struct BrowserState {
    /// Where the index and the user's file live.
    dir: Option<PathBuf>,
    loaded: bool,
    index: Index,
    user: UserData,
    index_changed: Option<Instant>,
    user_changed: Option<Instant>,
    place: Place,
    folder: Option<PathBuf>,
    search: String,
    filter: BrowserFilter,
    sort: (SortBy, bool),
    similar: Option<PathBuf>,
    selected: Vec<PathBuf>,
    listing: Listing,
    serial: u64,
    /// Listing asked for and not come yet.
    listing_pending: bool,
    rows: Vec<BrowserRow>,
    rows_dirty: bool,
    scanner: Option<Scanner>,
    preview: Option<Previewing>,
    placing: Vec<Placing>,
}

impl Default for BrowserState {
    fn default() -> Self {
        Self {
            dir: None,
            loaded: false,
            index: Index::fresh(),
            user: UserData::default(),
            index_changed: None,
            user_changed: None,
            place: Place::ProjectMedia,
            folder: None,
            search: String::new(),
            filter: BrowserFilter::default(),
            sort: (SortBy::Name, false),
            similar: None,
            selected: Vec::new(),
            listing: Listing::default(),
            serial: 0,
            listing_pending: false,
            rows: Vec::new(),
            rows_dirty: true,
            scanner: None,
            preview: None,
            placing: Vec::new(),
        }
    }
}

impl BrowserState {
    pub fn rows(&self) -> &[BrowserRow] {
        &self.rows
    }

    pub fn place(&self) -> &Place {
        &self.place
    }

    /// The folder looked in (`None`: the Recent list).
    pub fn folder(&self) -> Option<&Path> {
        self.folder.as_deref()
    }

    pub fn search(&self) -> &str {
        &self.search
    }

    pub fn filter(&self) -> &BrowserFilter {
        &self.filter
    }

    /// The sort and whether it is the other way round.
    pub fn sort(&self) -> (SortBy, bool) {
        self.sort
    }

    pub fn deep(&self) -> bool {
        self.user.deep
    }

    pub fn similar_to(&self) -> Option<&Path> {
        self.similar.as_deref()
    }

    pub fn selected(&self) -> &[PathBuf] {
        &self.selected
    }

    pub fn settings(&self) -> PreviewSettings {
        self.user.preview
    }

    pub fn user_folders(&self) -> &[PathBuf] {
        &self.user.folders
    }

    pub fn all_tags(&self) -> Vec<String> {
        self.user.all_tags().into_iter().collect()
    }

    pub fn tags_of(&self, path: &Path) -> Vec<String> {
        self.user.tags_of(path).cloned().collect()
    }

    pub fn rating(&self, path: &Path) -> u8 {
        self.user.rating(path)
    }

    /// What the index knows of `path` (while the file is as it was).
    pub fn entry(&self, path: &Path) -> Option<&Entry> {
        let stamp = index::Stamp::read(path)?;
        self.index.current(path, stamp)
    }

    /// The listing was cut short (more than [`scan::MAX_LISTED`] files).
    pub fn truncated(&self) -> bool {
        self.listing.truncated
    }

    pub fn error(&self) -> Option<&str> {
        self.listing.error.as_deref()
    }

    /// Files still to read and to analyse.
    pub fn pending(&self) -> (usize, usize) {
        self.scanner.as_ref().map_or((0, 0), Scanner::pending)
    }

    pub fn previewing(&self) -> Option<&Path> {
        self.preview.as_ref().map(|p| p.path.as_path())
    }

    /// The row for `path`.
    pub fn row(&self, path: &Path) -> Option<&BrowserRow> {
        self.rows.iter().find(|r| r.path == path)
    }
}

/// The ionian root a key shares its notes with (relative keys alike).
fn ionian(k: Key) -> u8 {
    match k.scale {
        Scale::Minor
        | Scale::HarmonicMinor
        | Scale::MelodicMinor
        | Scale::MinorPentatonic
        | Scale::Blues => (k.root + 3) % 12,
        Scale::Dorian => (k.root + 10) % 12,
        Scale::Phrygian => (k.root + 8) % 12,
        Scale::Lydian => (k.root + 7) % 12,
        Scale::Mixolydian => (k.root + 5) % 12,
        Scale::Locrian => (k.root + 1) % 12,
        _ => k.root,
    }
}

/// Semitones from `from` to `to` (−5…+6; relative keys are 0).
pub fn transposition(from: Key, to: Key) -> i32 {
    let d = (i32::from(ionian(to)) - i32::from(ionian(from))).rem_euclid(12);
    if d > 6 { d - 12 } else { d }
}

/// Keys that sit together: the same notes, or a fifth apart.
pub fn keys_fit(a: Key, b: Key) -> bool {
    matches!(transposition(a, b).abs(), 0 | 5 | 7)
}

/// `bpm` doubled or halved to the nearest of `to`.
fn nearest_tempo(bpm: f64, to: f64) -> f64 {
    [bpm / 2.0, bpm, bpm * 2.0]
        .into_iter()
        .min_by(|a, b| (a / to).ln().abs().total_cmp(&(b / to).ln().abs()))
        .unwrap_or(bpm)
}

/// Audio in time: an even number of beats at its tempo (within 3 %).
pub fn is_loop(bpm: Option<f64>, seconds: Option<f64>) -> bool {
    let (Some(bpm), Some(s)) = (bpm, seconds) else {
        return false;
    };
    let beats = s * bpm / 60.0;
    let whole = beats.round();
    whole >= 2.0 && whole as i64 % 2 == 0 && (beats - whole).abs() <= whole * 0.03
}

fn format_text(f: &Facts) -> String {
    match f.bits {
        Some(b) => format!("{} {b}-bit", f.format),
        None => f.format.clone(),
    }
}

impl Session {
    // --- reading ------------------------------------------------------------------

    pub fn browser(&self) -> &BrowserState {
        &self.browser
    }

    /// The index and the user's file live in `dir` from now on (tests;
    /// `<data>/browser` otherwise). Read anew.
    pub fn set_browser_dir(&mut self, dir: PathBuf) {
        self.flush_browser();
        self.browser.dir = Some(dir);
        self.browser.loaded = false;
        self.browser_loaded();
    }

    fn browser_dir(&self) -> PathBuf {
        self.browser
            .dir
            .clone()
            .unwrap_or_else(|| crate::media::data_dir().join("browser"))
    }

    fn browser_loaded(&mut self) {
        if self.browser.loaded {
            return;
        }
        let dir = self.browser_dir();
        self.browser.index = Index::load(&dir.join("index.json"));
        self.browser.user = UserData::load(&dir.join("user.json"));
        self.browser.loaded = true;
        if self.browser.folder.is_none() && self.browser.place != Place::Recent {
            let place = if self.media_dir.is_dir() {
                Place::ProjectMedia
            } else {
                Place::Home
            };
            self.open_place(place);
        }
        self.browser.rows_dirty = true;
    }

    /// The places listed: the fixed ones, then the user's folders.
    pub fn browser_places(&self) -> Vec<Place> {
        let mut out = vec![
            Place::ProjectMedia,
            Place::Samples,
            Place::Recent,
            Place::Home,
        ];
        out.extend(self.browser.user.folders.iter().cloned().map(Place::Folder));
        out
    }

    /// A place's top folder (`None`: Recent, or no home).
    pub fn browser_place_root(&self, place: &Place) -> Option<PathBuf> {
        match place {
            Place::ProjectMedia => Some(self.media_dir.clone()),
            Place::Samples => Some(self.media_dir.join(crate::samples::SAMPLES_FOLDER)),
            Place::Recent => None,
            Place::Home => home(),
            Place::Folder(p) => Some(p.clone()),
        }
    }

    /// What a file would be changed by to match the project at `at` with
    /// the browser's settings (`None`: nothing to match).
    pub fn browser_fit(&self, path: &Path, at: MusicalTime) -> Option<Fit> {
        let settings = self.browser.user.preview;
        let entry = self.browser.entry(path)?;
        let a = entry.analysis.as_ref()?;
        let project_bpm = self.project.timeline.tempo.bpm_at(at);
        let tempo = (settings.sync && entry.kind == FileKind::Audio)
            .then_some(a.bpm)
            .flatten()
            .filter(|_| is_loop(a.bpm, entry.seconds()))
            .map(|b| nearest_tempo(b, project_bpm))
            .filter(|b| (0.5..=2.0).contains(&(project_bpm / b)));
        let semitones = match (settings.key, a.key, self.project.key_at(at)) {
            (true, Some(from), Some(to)) => transposition(from, to),
            _ => 0,
        };
        (tempo.is_some() || semitones != 0).then_some(Fit {
            bpm: tempo,
            project_bpm,
            semitones,
        })
    }

    /// The preview heard now.
    pub fn browser_preview(&self) -> Option<PreviewInfo> {
        let p = self.browser.preview.as_ref()?;
        let position = match &p.sound {
            Sound::Audio {
                audio, loop_frames, ..
            } => {
                let shared = self.engine.listening().link.preview().shared();
                let len = if *loop_frames > 0 {
                    Some(*loop_frames)
                } else {
                    audio.frames()
                };
                shared
                    .position()
                    .zip(len.filter(|l| *l > 0))
                    .map(|(at, len)| (at.rem_euclid(len)) as f64 / len as f64)
            }
            Sound::Midi { status, length } => status
                .position()
                .map(|at| at.rem_euclid((*length).max(1)) as f64 / (*length).max(1) as f64),
        };
        let (frames, loop_frames) = match &p.sound {
            Sound::Audio {
                audio, loop_frames, ..
            } => (audio.frames(), *loop_frames),
            Sound::Midi { length, .. } => (Some(*length), *length),
        };
        Some(PreviewInfo {
            path: p.path.clone(),
            name: name_of(&p.path),
            position,
            fit: p.fit.map(|f| f.text()).unwrap_or_default(),
            midi: matches!(p.sound, Sound::Midi { .. }),
            frames,
            loop_frames,
            synced: p.synced,
        })
    }

    /// The browser's worker has nothing left to do and every file placed
    /// is placed (tests, scripts). Saves the index.
    pub fn wait_for_browser(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            self.tick_browser();
            let idle = !self.browser.listing_pending
                && self.browser.scanner.as_ref().is_none_or(Scanner::idle)
                && self.browser.placing.is_empty();
            if idle {
                // What the worker sent last.
                self.tick_browser();
                self.flush_browser();
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Wait for the preview's audio to be made (tests).
    pub fn wait_for_preview_audio(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            let done = match self.browser.preview.as_ref().map(|p| &p.sound) {
                Some(Sound::Audio { audio, .. }) => audio.is_done(),
                _ => true,
            };
            if done {
                self.tick_browser();
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    // --- acting -------------------------------------------------------------------

    pub(crate) fn browser_op(&mut self, op: BrowserOp) -> Result<()> {
        self.browser_loaded();
        match op {
            BrowserOp::Open(place) => self.open_place(place),
            BrowserOp::Enter(dir) => {
                if dir.is_dir() {
                    self.browser.folder = Some(dir);
                    self.browser.similar = None;
                    self.list_folder();
                }
            }
            BrowserOp::Up => {
                let root = self.browser_place_root(&self.browser.place);
                if let Some(f) = self.browser.folder.clone()
                    && Some(&f) != root.as_ref()
                    && let Some(up) = f.parent()
                {
                    self.browser.folder = Some(up.to_path_buf());
                    self.list_folder();
                }
            }
            BrowserOp::Refresh => self.list_folder(),
            BrowserOp::Search(text) => {
                let was_empty = self.browser.search.trim().is_empty();
                self.browser.search = text;
                // Searching goes through the folders inside.
                if was_empty != self.browser.search.trim().is_empty() && self.browser.user.deep {
                    self.list_folder();
                }
                self.browser.rows_dirty = true;
            }
            BrowserOp::Filter(f) => {
                self.browser.filter = f;
                self.browser.rows_dirty = true;
            }
            BrowserOp::Sort(by) => {
                let (old, back) = self.browser.sort;
                self.browser.sort = (
                    by,
                    if old == by {
                        !back
                    } else {
                        by == SortBy::Rating
                    },
                );
                self.browser.rows_dirty = true;
            }
            BrowserOp::Deep(on) => {
                self.browser.user.deep = on;
                self.user_changed();
                if !self.browser.search.trim().is_empty() {
                    self.list_folder();
                }
            }
            BrowserOp::AddFolder(dir) => {
                if !dir.is_dir() {
                    return Err(SessionError::Other(format!(
                        "{} is not a folder",
                        dir.display()
                    )));
                }
                if !self.browser.user.folders.contains(&dir) {
                    self.browser.user.folders.push(dir.clone());
                    self.user_changed();
                }
                self.open_place(Place::Folder(dir));
            }
            BrowserOp::RemoveFolder(dir) => {
                self.browser.user.folders.retain(|f| *f != dir);
                self.user_changed();
                if self.browser.place == Place::Folder(dir) {
                    self.open_place(Place::ProjectMedia);
                }
            }
            BrowserOp::Select(paths) => {
                let auto = self.browser.user.preview.auto;
                let one = (paths.len() == 1).then(|| paths[0].clone());
                self.browser.selected = paths;
                if auto
                    && let Some(p) = one
                    && FileKind::of(&p).is_some()
                    && p.is_file()
                {
                    self.preview_file(&p)?;
                }
            }
            BrowserOp::Preview(path) => self.preview_file(&path)?,
            BrowserOp::StopPreview => self.stop_preview(),
            BrowserOp::Settings(s) => {
                let volume = s.volume_db != self.browser.user.preview.volume_db;
                self.browser.user.preview = PreviewSettings {
                    volume_db: if s.volume_db.is_finite() {
                        s.volume_db.clamp(-60.0, 12.0)
                    } else {
                        0.0
                    },
                    ..s
                };
                self.user_changed();
                if volume {
                    self.apply_preview_volume();
                }
                self.browser.rows_dirty = true;
            }
            BrowserOp::Rate { paths, stars } => {
                for p in paths {
                    if stars == 0 {
                        self.browser.user.ratings.remove(&p);
                    } else {
                        self.browser.user.ratings.insert(p, stars.min(MAX_RATING));
                    }
                }
                self.user_changed();
            }
            BrowserOp::Tag { paths, tag, on } => {
                let tag = tag.trim().to_string();
                if tag.is_empty() {
                    return Ok(());
                }
                for p in paths {
                    let set = self.browser.user.tags.entry(p.clone()).or_default();
                    if on {
                        set.insert(tag.clone());
                    } else {
                        set.remove(&tag);
                        if set.is_empty() {
                            self.browser.user.tags.remove(&p);
                        }
                    }
                }
                self.user_changed();
            }
            BrowserOp::AskTag(paths) => {
                if !paths.is_empty() {
                    self.ui_requests
                        .push(crate::UiRequest::NewBrowserTag(paths));
                }
            }
            BrowserOp::FindSimilar(path) => {
                let kind = FileKind::of(&path).ok_or_else(|| {
                    SessionError::Other(format!("{} is not a sound", path.display()))
                })?;
                if kind != FileKind::Audio {
                    return Err(SessionError::Other(
                        "Find Similar listens to sound files".into(),
                    ));
                }
                let stamp = index::Stamp::read(&path)
                    .ok_or_else(|| SessionError::Other(format!("{} is gone", path.display())))?;
                let (read, analyse) = self.browser.index.note(&path, stamp, kind);
                if analyse {
                    self.scanner().first(path.clone(), kind, read);
                }
                self.browser.similar = Some(path);
                self.browser.rows_dirty = true;
            }
            BrowserOp::ClearSimilar => {
                self.browser.similar = None;
                self.browser.rows_dirty = true;
            }
            BrowserOp::Analyse(paths) => {
                if paths.is_empty() {
                    // Everything here and below, read and analysed.
                    if let Some(dir) = self.browser.folder.clone() {
                        self.browser.serial += 1;
                        let serial = self.browser.serial;
                        self.browser.listing_pending = true;
                        self.scanner().list(dir, true, serial);
                        self.browser.listing.serial = u64::MAX;
                    }
                } else {
                    for p in paths {
                        self.browser.index.entries.remove(&p);
                        if let (Some(kind), Some(stamp)) =
                            (FileKind::of(&p), index::Stamp::read(&p))
                        {
                            self.browser.index.note(&p, stamp, kind);
                            self.scanner().first(p, kind, true);
                        }
                    }
                }
                self.browser.rows_dirty = true;
            }
            BrowserOp::Place { files, track, at } => {
                self.place_files(files, Destination::Arranger, track, at)?;
            }
            BrowserOp::ToLauncher {
                files,
                track,
                scene,
            } => {
                self.place_files(
                    files,
                    Destination::Launcher { scene },
                    Some(track),
                    MusicalTime::ZERO,
                )?;
            }
        }
        self.revision += 1;
        Ok(())
    }

    fn scanner(&mut self) -> &Scanner {
        self.browser.scanner.get_or_insert_with(Scanner::start)
    }

    fn user_changed(&mut self) {
        self.browser.user_changed = Some(Instant::now());
        self.browser.rows_dirty = true;
    }

    fn open_place(&mut self, place: Place) {
        self.browser.folder = self.browser_place_root(&place);
        self.browser.place = place;
        self.browser.similar = None;
        self.list_folder();
    }

    /// Ask for the folder's listing (deep while searching).
    fn list_folder(&mut self) {
        self.browser.rows_dirty = true;
        // The project's media moved (its first save): its place follows.
        if matches!(self.browser.place, Place::ProjectMedia | Place::Samples)
            && self.browser.folder.as_ref().is_some_and(|f| !f.exists())
        {
            self.browser.folder = self.browser_place_root(&self.browser.place);
        }
        let Some(dir) = self.browser.folder.clone() else {
            // Recent: the files themselves.
            self.browser.listing = Listing::default();
            self.browser.listing_pending = false;
            self.queue_recent();
            return;
        };
        let deep = self.browser.user.deep && !self.browser.search.trim().is_empty();
        self.browser.serial += 1;
        let serial = self.browser.serial;
        self.browser.listing_pending = true;
        self.scanner().list(dir, deep, serial);
    }

    fn queue_recent(&mut self) {
        let files: Vec<(PathBuf, index::Stamp, FileKind)> = self
            .browser
            .user
            .recent
            .iter()
            .filter_map(|p| Some((p.clone(), index::Stamp::read(p)?, FileKind::of(p)?)))
            .collect();
        self.browser.listing.files = files;
        self.queue_listing();
    }

    /// Read and analyse what the listing holds that the index does not
    /// know (files the search passes first, at most [`MAX_QUEUED`]).
    fn queue_listing(&mut self) {
        let words = search_words(&self.browser.search);
        let all = self.browser.listing.serial == u64::MAX;
        let mut read = Vec::new();
        let mut analyse = Vec::new();
        let files = self.browser.listing.files.clone();
        for (path, stamp, kind) in files {
            let (r, a) = self.browser.index.note(&path, stamp, kind);
            if !all
                && !matches_words(
                    &words,
                    &path,
                    self.browser.folder.as_deref(),
                    &self.browser.user,
                )
            {
                continue;
            }
            if r {
                read.push((path.clone(), kind));
            }
            if a && (all || analyse.len() < MAX_QUEUED) {
                analyse.push((path, kind));
            }
        }
        self.browser.index_changed.get_or_insert_with(Instant::now);
        if read.is_empty() && analyse.is_empty() {
            if let Some(s) = &self.browser.scanner {
                s.want(Vec::new(), Vec::new());
            }
            return;
        }
        self.scanner().want(read, analyse);
    }

    /// Take what the worker found, finish placements and previews, keep
    /// the index written, the rows up to date (from the session's tick).
    pub(crate) fn tick_browser(&mut self) {
        let found = self
            .browser
            .scanner
            .as_ref()
            .map(Scanner::take)
            .unwrap_or_default();
        let mut changed = !found.is_empty();
        for f in found {
            self.take_found(f);
        }
        // Placements imported.
        let mut i = 0;
        while i < self.browser.placing.len() {
            if self.browser.placing[i].job.is_finished() {
                let p = self.browser.placing.swap_remove(i);
                if let Err(e) = self.finish_placing(p) {
                    self.notify(NoticeLevel::Error, format!("placing: {e}"));
                }
                changed = true;
            } else {
                i += 1;
            }
        }
        changed |= self.poll_preview();
        if self.browser.rows_dirty {
            self.make_rows();
            changed = true;
        }
        let now = Instant::now();
        let due = |t: Option<Instant>| t.is_some_and(|t| now.duration_since(t) >= SAVE_AFTER);
        if due(self.browser.index_changed) || due(self.browser.user_changed) {
            self.flush_browser();
        }
        if changed {
            self.shown_changed();
        }
    }

    /// Write what changed (index, the user's file).
    pub(crate) fn flush_browser(&mut self) {
        if !self.browser.loaded {
            return;
        }
        let dir = self.browser_dir();
        if self.browser.index_changed.take().is_some() {
            self.browser.index.prune();
            if let Err(e) = self.browser.index.save(&dir.join("index.json")) {
                tracing::warn!("media browser index: {e}");
            }
        }
        if self.browser.user_changed.take().is_some()
            && let Err(e) = self.browser.user.save(&dir.join("user.json"))
        {
            self.notify(NoticeLevel::Warning, format!("media browser: {e}"));
        }
    }

    fn take_found(&mut self, f: Found) {
        match f {
            Found::Listed(mut l) => {
                // Self-freezing's own files are not media to browse.
                let cache = self.cache_dir();
                l.folders.retain(|p| *p != cache);
                l.files.retain(|(p, ..)| !p.starts_with(&cache));
                let explicit = self.browser.listing.serial == u64::MAX;
                if l.serial != self.browser.serial {
                    return;
                }
                self.browser.listing_pending = false;
                if explicit && l.deep {
                    // "Analyse everything here": read and analyse all, the
                    // listing shown stays.
                    let shown = std::mem::replace(&mut self.browser.listing, l);
                    self.browser.listing.serial = u64::MAX;
                    self.queue_listing();
                    self.browser.listing = shown;
                    self.browser.listing.serial = self.browser.serial;
                } else {
                    self.browser.listing = l;
                    self.queue_listing();
                }
                self.browser.rows_dirty = true;
            }
            Found::Read { path, stamp, facts } => {
                if let Some(e) = self.browser.index.entries.get_mut(&path)
                    && e.stamp == stamp
                {
                    match facts {
                        Ok(f) => e.facts = Some(f),
                        Err(_) => e.unreadable = true,
                    }
                    self.browser.index_changed.get_or_insert_with(Instant::now);
                    self.browser.rows_dirty = true;
                }
            }
            Found::Analysed {
                path,
                stamp,
                analysis,
            } => {
                if let Some(e) = self.browser.index.entries.get_mut(&path)
                    && e.stamp == stamp
                {
                    match analysis {
                        Ok(a) => e.analysis = Some(a),
                        Err(_) => e.unreadable = true,
                    }
                    self.browser.index_changed.get_or_insert_with(Instant::now);
                    self.browser.rows_dirty = true;
                }
            }
        }
    }

    fn row_of(&self, path: &Path, kind: FileKind) -> BrowserRow {
        let b = &self.browser;
        let stamp = b.index.entries.get(path).map(|e| e.stamp);
        let e = stamp.and_then(|s| b.index.current(path, s));
        let facts = e.and_then(|e| e.facts.as_ref());
        let a = e.and_then(|e| e.analysis.as_ref());
        let seconds = e.and_then(Entry::seconds);
        BrowserRow {
            path: path.to_path_buf(),
            name: name_of(path),
            item: match kind {
                FileKind::Audio => RowItem::Audio,
                FileKind::Midi => RowItem::Midi,
            },
            format: facts.map(format_text).unwrap_or_default(),
            seconds,
            channels: facts.map_or(0, |f| f.channels),
            rate: facts.map_or(0, |f| f.rate),
            bpm: a.and_then(|a| a.bpm),
            key: a.and_then(|a| a.key),
            rating: b.user.rating(path),
            tags: b.user.tags_of(path).cloned().collect(),
            thumb: a.map(|a| a.thumb.clone()).unwrap_or_default(),
            is_loop: kind == FileKind::Audio && is_loop(a.and_then(|a| a.bpm), seconds),
            pending: e.is_none_or(|e| !e.unreadable && e.analysis.is_none()),
            likeness: None,
        }
    }

    fn admits(&self, r: &BrowserRow) -> bool {
        let f = &self.browser.filter;
        let kind = match f.kind {
            KindFilter::All => true,
            KindFilter::Audio => r.item == RowItem::Audio,
            KindFilter::Midi => r.item == RowItem::Midi,
            KindFilter::Loops => r.item == RowItem::Audio && r.is_loop,
            KindFilter::OneShots => {
                r.item == RowItem::Audio
                    && !r.pending
                    && !r.is_loop
                    && r.seconds.is_some_and(|s| s < 6.0)
            }
        };
        if !kind || !f.length.admits(r.seconds) || r.rating < f.rating {
            return false;
        }
        if let Some(t) = &f.tag
            && !r.tags.contains(t)
        {
            return false;
        }
        let here = self.playhead();
        let key = match f.key {
            KeyFilter::Any => true,
            KeyFilter::Project => match (r.key, self.project.key_at(here)) {
                (Some(k), Some(p)) => keys_fit(k, p),
                (_, None) => true,
                _ => false,
            },
            KeyFilter::Key(want) => r.key.is_some_and(|k| transposition(k, want) == 0),
        };
        let tempo = match f.tempo {
            TempoFilter::Any => true,
            TempoFilter::Project => r.bpm.is_some_and(|b| {
                let p = self.project.timeline.tempo.bpm_at(here);
                (nearest_tempo(b, p) / p - 1.0).abs() <= TEMPO_REACH
            }),
            TempoFilter::Range(lo, hi) => r.bpm.is_some_and(|b| b >= lo && b <= hi),
        };
        key && tempo
    }

    /// The list as shown (from the listing or Find Similar, filtered and
    /// sorted).
    fn make_rows(&mut self) {
        self.browser.rows_dirty = false;
        let mut rows = Vec::new();
        if let Some(target) = self.browser.similar.clone() {
            let print = self
                .browser
                .entry(&target)
                .and_then(|e| e.analysis.as_ref())
                .and_then(Analysis::sound_print);
            if let Some(print) = print {
                let mut near: Vec<(f32, PathBuf)> = self
                    .browser
                    .index
                    .entries
                    .iter()
                    .filter(|(p, e)| **p != target && e.kind == FileKind::Audio)
                    .filter_map(|(p, e)| {
                        let other = e.analysis.as_ref()?.sound_print()?;
                        Some((sound_print::distance(&print, &other), p.clone()))
                    })
                    .collect();
                near.sort_by(|a, b| a.0.total_cmp(&b.0));
                for (d, p) in near
                    .into_iter()
                    .filter(|(_, p)| p.is_file())
                    .take(MAX_SIMILAR)
                {
                    let mut r = self.row_of(&p, FileKind::Audio);
                    r.likeness = Some(1.0 / (1.0 + d));
                    rows.push(r);
                }
            }
            self.browser.rows = rows;
            return;
        }
        let words = search_words(&self.browser.search);
        let root = self.browser_place_root(&self.browser.place);
        if let Some(f) = &self.browser.folder
            && Some(f) != root.as_ref()
        {
            rows.push(BrowserRow {
                path: f.parent().map(Path::to_path_buf).unwrap_or_default(),
                name: "..".into(),
                item: RowItem::Up,
                format: String::new(),
                seconds: None,
                channels: 0,
                rate: 0,
                bpm: None,
                key: None,
                rating: 0,
                tags: Vec::new(),
                thumb: String::new(),
                is_loop: false,
                pending: false,
                likeness: None,
            });
        }
        if words.is_empty() && self.browser.filter.is_any() {
            for d in &self.browser.listing.folders {
                rows.push(BrowserRow {
                    path: d.clone(),
                    name: name_of(d),
                    item: RowItem::Folder,
                    format: String::new(),
                    seconds: None,
                    channels: 0,
                    rate: 0,
                    bpm: None,
                    key: None,
                    rating: 0,
                    tags: Vec::new(),
                    thumb: String::new(),
                    is_loop: false,
                    pending: false,
                    likeness: None,
                });
            }
        }
        let first_file = rows.len();
        for (path, _, kind) in &self.browser.listing.files {
            if !matches_words(
                &words,
                path,
                self.browser.folder.as_deref(),
                &self.browser.user,
            ) {
                continue;
            }
            let r = self.row_of(path, *kind);
            if self.admits(&r) {
                rows.push(r);
            }
        }
        let (by, back) = self.browser.sort;
        // Recent files stay newest first unless sorted otherwise.
        if self.browser.place == Place::Recent && by == SortBy::Name && !back {
            self.browser.rows = rows;
            return;
        }
        let files = &mut rows[first_file..];
        let name = |r: &BrowserRow| r.name.to_lowercase();
        files.sort_by(|a, b| {
            let o = match by {
                SortBy::Name => name(a).cmp(&name(b)),
                SortBy::Kind => (a.item as u8, &a.format).cmp(&(b.item as u8, &b.format)),
                SortBy::Length => cmp_opt(a.seconds, b.seconds),
                SortBy::Tempo => cmp_opt(a.bpm, b.bpm),
                SortBy::Key => a
                    .key
                    .map(|k| (ionian(k), k.root))
                    .cmp(&b.key.map(|k| (ionian(k), k.root))),
                SortBy::Rating => a.rating.cmp(&b.rating),
            };
            let o = if back { o.reverse() } else { o };
            o.then_with(|| name(a).cmp(&name(b)))
        });
        self.browser.rows = rows;
    }

    // --- previews -----------------------------------------------------------------

    fn stop_preview(&mut self) {
        let Some(p) = self.browser.preview.take() else {
            return;
        };
        match p.sound {
            Sound::Audio { job, .. } => {
                if let Some(j) = job {
                    j.cancel();
                }
                self.engine.listening().link.preview().set(None);
            }
            Sound::Midi { .. } => {
                if let Err(e) = self.engine.set_midi_preview(None) {
                    tracing::warn!("media browser: {e}");
                }
            }
        }
        self.shown_changed();
    }

    fn apply_preview_volume(&self) {
        let g = faderframe_core::db_to_gain(self.browser.user.preview.volume_db);
        self.engine.listening().link.preview().shared().set_gain(g);
    }

    /// Hear `path` (the preview playing stops).
    fn preview_file(&mut self, path: &Path) -> Result<()> {
        let kind = FileKind::of(path)
            .ok_or_else(|| SessionError::Other(format!("{} is not a sound", path.display())))?;
        if !path.is_file() {
            return Err(SessionError::Other(format!("{} is gone", path.display())));
        }
        self.stop_preview();
        let settings = self.browser.user.preview;
        let here = self.playhead();
        let fit = self.browser_fit(path, here);
        let rate = match self.engine.stream_rate() {
            0 => self.engine.sample_rate(),
            r => r,
        };
        let tl = &self.project.timeline;
        let srate = f64::from(rate);
        let playing = self.transport.playing;
        // The next bar ahead of what is processed.
        let next_bar = || {
            let pos = self.processing_position() + (START_LEAD * srate) as i64;
            let at = tl.to_musical(pos.max(0), srate);
            let mut bar = tl.meter.bar_at(at);
            if tl.meter.bar_start(bar) < at {
                bar += 1;
            }
            (
                tl.to_samples(tl.meter.bar_start(bar), srate),
                tl.meter.bar_start(bar),
            )
        };
        let bar_frames = |from: MusicalTime| {
            let bar = tl.meter.bar_at(from);
            (tl.to_samples(tl.meter.bar_start(bar + 1), srate)
                - tl.to_samples(tl.meter.bar_start(bar), srate))
            .max(1)
        };
        match kind {
            FileKind::Audio => {
                let entry = self.browser.entry(path);
                let seconds = entry.and_then(Entry::seconds);
                let print = entry
                    .and_then(|e| e.analysis.as_ref())
                    .and_then(Analysis::sound_print);
                let synced = fit.is_some_and(|f| f.bpm.is_some());
                let change = fit.map_or(Change::NONE, |f| Change {
                    speed: f.speed(),
                    semitones: f.semitones as f32,
                    rhythmic: print.as_ref().is_some_and(|p| p.onsets > 2.0),
                });
                let (start, at) = if synced && playing {
                    let (s, m) = next_bar();
                    (PreviewStart::At(s), m)
                } else {
                    (PreviewStart::Now, here)
                };
                let looped = settings.looped && synced;
                let loop_frames = match seconds {
                    Some(s) if looped => {
                        let out = s * srate / change.speed;
                        let bar = bar_frames(at) as f64;
                        ((out / bar).round().max(1.0) * bar) as i64
                    }
                    _ => 0,
                };
                let (audio, writer) =
                    PreviewAudio::new(rate, 2, (srate * preview::MAX_PREVIEW_SECONDS) as usize);
                self.apply_preview_volume();
                let link = Arc::clone(self.engine.listening().link.preview());
                let generation = link.set(Some(PreviewSound {
                    audio: Arc::clone(&audio),
                    start,
                    looped,
                    loop_frames,
                }));
                let job = PreviewJob::spawn(path.to_path_buf(), rate, change, writer);
                self.browser.preview = Some(Previewing {
                    path: path.to_path_buf(),
                    fit,
                    synced: matches!(start, PreviewStart::At(_)),
                    sound: Sound::Audio {
                        generation,
                        audio,
                        job: Some(job),
                        loop_frames,
                    },
                });
            }
            FileKind::Midi => {
                let notes = smf::read(path)
                    .map_err(|e| SessionError::Other(format!("{}: {e}", path.display())))?;
                let track = self.preview_instrument().ok_or_else(|| {
                    SessionError::Other(
                        "select an instrument track to hear MIDI files through".into(),
                    )
                })?;
                let semitones = fit.map_or(0, |f| f.semitones);
                let (start, from) = if settings.sync && playing {
                    let (s, m) = next_bar();
                    (Some(PreviewStart::At(s)), m)
                } else {
                    (Some(PreviewStart::Now), here)
                };
                let frame_of = |q: f64| -> i64 {
                    if settings.sync {
                        tl.to_samples(from + MusicalTime::from_quarters(q), srate)
                            - tl.to_samples(from, srate)
                    } else {
                        (notes.seconds_at(q) * srate).round() as i64
                    }
                };
                let events = notes
                    .events
                    .iter()
                    .map(|&(q, mut b)| {
                        // Drums (channel 10) stay where they are.
                        if semitones != 0 && b[0] & 0xE0 == 0x80 && b[0] & 15 != 9 {
                            let k = (i32::from(b[1]) + semitones).clamp(0, 127);
                            b[1] = k as u8;
                        }
                        (frame_of(q), b)
                    })
                    .collect();
                let length = frame_of(notes.bar_quarters()).max(1);
                let project_bpm = tl.tempo.bpm_at(here);
                self.engine
                    .midi_shared()
                    .audition_track
                    .store(track.raw(), std::sync::atomic::Ordering::Relaxed);
                // Played by hand: from its chain, not a cache.
                self.wake_cached(track);
                let status = self
                    .engine
                    .set_midi_preview(Some(MidiPreview {
                        events,
                        length,
                        looped: settings.looped,
                        start,
                    }))
                    .map_err(|e| SessionError::Other(e.to_string()))?
                    .ok_or_else(|| SessionError::Other("the MIDI preview did not start".into()))?;
                self.browser.preview = Some(Previewing {
                    path: path.to_path_buf(),
                    synced: matches!(start, Some(PreviewStart::At(_))),
                    fit: (semitones != 0).then_some(Fit {
                        bpm: None,
                        project_bpm,
                        semitones,
                    }),
                    sound: Sound::Midi { status, length },
                });
            }
        }
        // Heard: first among the recent files.
        self.browser.user.used(path);
        self.user_changed();
        self.shown_changed();
        Ok(())
    }

    /// The track MIDI files preview through: the selected instrument (or
    /// MIDI) track, else the first instrument track.
    fn preview_instrument(&self) -> Option<TrackId> {
        let plays = |t: &Track| matches!(t.kind, TrackKind::Instrument | TrackKind::Midi);
        self.selection
            .tracks
            .iter()
            .filter_map(|id| self.project.track(*id))
            .find(|t| plays(t))
            .or_else(|| {
                self.project
                    .tracks
                    .iter()
                    .find(|t| t.kind == TrackKind::Instrument)
            })
            .map(|t| t.id)
    }

    /// The preview's worker and end (`true`: what is shown changed).
    fn poll_preview(&mut self) -> bool {
        let Some(p) = self.browser.preview.as_mut() else {
            return false;
        };
        let mut error = None;
        let ended = match &mut p.sound {
            Sound::Audio {
                generation, job, ..
            } => {
                if job.as_ref().is_some_and(PreviewJob::is_finished)
                    && let Some(j) = job.take()
                    && let Err(e) = j.join()
                {
                    error = Some(e);
                }
                self.engine
                    .listening()
                    .link
                    .preview()
                    .shared()
                    .has_ended(*generation)
            }
            Sound::Midi { status, .. } => status.has_ended(),
        };
        if let Some(e) = error {
            self.notify(NoticeLevel::Warning, format!("preview: {e}"));
            self.stop_preview();
            return true;
        }
        // Ended: gone (a MIDI preview's run stopped in the engine too).
        if ended
            && let Some(Previewing {
                sound: Sound::Midi { .. },
                ..
            }) = self.browser.preview.take()
            && let Err(e) = self.engine.set_midi_preview(None)
        {
            tracing::warn!("media browser: {e}");
        }
        // (While one plays its place moves.)
        true
    }

    // --- placing ------------------------------------------------------------------

    fn place_files(
        &mut self,
        files: Vec<PathBuf>,
        to: Destination,
        track: Option<TrackId>,
        at: MusicalTime,
    ) -> Result<()> {
        let (midi, audio): (Vec<PathBuf>, Vec<PathBuf>) = files
            .into_iter()
            .filter(|f| f.is_file())
            .partition(|f| crate::is_midi_file(f));
        for f in &midi {
            match to {
                Destination::Arranger => {
                    self.import_midi_file(f, at, false)?;
                    self.browser.user.used(f);
                }
                Destination::Launcher { .. } => self.notify(
                    NoticeLevel::Warning,
                    "MIDI files go onto the arranger: drop them there",
                ),
            }
        }
        if audio.is_empty() {
            if midi.is_empty() {
                return Err(SessionError::Other("nothing to place".into()));
            }
            self.user_changed();
            return Ok(());
        }
        let here = match to {
            Destination::Arranger => at,
            Destination::Launcher { .. } => self.playhead(),
        };
        let mut fits = HashMap::new();
        let mut rhythmic = HashMap::new();
        for f in &audio {
            if let Some(fit) = self.browser_fit(f, here) {
                fits.insert(f.clone(), fit);
            }
            let drums = self
                .browser
                .entry(f)
                .and_then(|e| e.analysis.as_ref())
                .and_then(Analysis::sound_print)
                .is_some_and(|p: SoundPrint| p.onsets > 2.0);
            rhythmic.insert(f.clone(), drums);
            self.browser.user.used(f);
        }
        self.user_changed();
        let job = ImportJob::spawn(
            audio,
            self.media_dir.clone(),
            self.project.sample_rate,
            ImportTarget { track, at },
        );
        self.browser.placing.push(Placing {
            job,
            fits,
            rhythmic,
            to,
        });
        Ok(())
    }

    /// Imported: the clips placed (matched to the project), one undo step.
    fn finish_placing(&mut self, p: Placing) -> Result<()> {
        let target = p.job.target.clone();
        let results = p.job.join();
        let mut commands = Vec::new();
        let mut clips = Vec::new();
        let mut failures = Vec::new();
        let mut reuse = target.track.filter(|t| {
            self.project
                .track(*t)
                .is_some_and(|t| t.kind == TrackKind::Audio)
        });
        let launcher = matches!(p.to, Destination::Launcher { .. });
        if launcher && reuse.is_none() {
            return Err(SessionError::Other(
                "sounds go into the slots of audio tracks".into(),
            ));
        }
        let mut scenes = self.project.launcher.scenes.clone();
        let mut scene_index = match p.to {
            Destination::Launcher { scene, .. } => scenes.iter().position(|s| s.id == scene),
            Destination::Arranger => None,
        };
        let mut index = reuse
            .and_then(|t| self.project.track_index(t))
            .or_else(|| self.project.tracks.iter().rposition(|t| t.kind.has_clips()))
            .map_or(0, |i| i + 1);
        let mut colour = self.project.tracks.len();
        for r in results {
            let audio = match r {
                Ok(a) => a,
                Err((path, e)) => {
                    failures.push(format!("{}: {e}", path.display()));
                    continue;
                }
            };
            let source = AudioSource {
                id: self.project.ids.allocate(),
                name: audio.name.clone(),
                spec: SourceSpec::File {
                    path: audio.path.clone(),
                    channels: audio.channels as u16,
                    frames: audio.frames as i64,
                    sample_rate: audio.sample_rate,
                },
            };
            let span = source.frames(self.project.sample_rate);
            let fit = p.fits.get(&audio.original).copied();
            let track = match reuse {
                Some(t) if launcher || clips.is_empty() => t,
                _ => {
                    let id: TrackId = self.project.ids.allocate();
                    let layout = if audio.channels == 1 {
                        faderframe_core::ChannelLayout::Mono
                    } else {
                        faderframe_core::ChannelLayout::Stereo
                    };
                    let t = Track::new(
                        id,
                        TrackKind::Audio,
                        audio.name.clone(),
                        TrackColor::palette(colour),
                    )
                    .with_layout(layout);
                    colour += 1;
                    commands.push(Command::AddTrack {
                        track: Box::new(t),
                        index,
                    });
                    index += 1;
                    reuse = Some(id);
                    id
                }
            };
            // The tempo: warped to the project's on the arranger (a launcher
            // clip follows the tempo itself); the key: transposed.
            let warp = fit.filter(|_| !launcher).and_then(|f| {
                f.bpm.map(|_| {
                    let mut w = Warp::uniform(span);
                    if p.rhythmic.get(&audio.original).copied().unwrap_or(false) {
                        w.algorithm = WarpAlgorithm::Rhythmic;
                    }
                    w
                })
            });
            let length = match (&warp, fit) {
                (Some(_), Some(f)) => ((span as f64 / f.speed()).round() as i64).max(1),
                _ => span,
            };
            let clip = Clip {
                id: self.project.ids.allocate(),
                track,
                name: audio.name.clone(),
                color: None,
                start: if launcher {
                    MusicalTime::ZERO
                } else {
                    target.at
                },
                muted: false,
                content: ClipContent::Audio(AudioClip {
                    source: source.id,
                    source_offset: 0,
                    length,
                    gain_db: 0.0,
                    fades: Default::default(),
                    stretch: StretchSettings::pitch(fit.map_or(0, |f| f.semitones) as f32),
                    reversed: false,
                    warp,
                    pitch: None,
                    effects: None,
                    spectral: None,
                    envelope: None,
                }),
            };
            clips.push(clip.id);
            match crate::media::open_stream(&audio.path) {
                Ok(s) => {
                    self.sources
                        .insert(source.id, faderframe_engine::Source::Stream(s));
                }
                Err(e) => failures.push(format!("{}: {e}", audio.path.display())),
            }
            self.peaks.insert(source.id, Arc::new(audio.peaks));
            commands.push(Command::AddSource {
                source: Box::new(source),
            });
            match p.to {
                Destination::Arranger => commands.push(Command::AddClip {
                    clip: Box::new(clip),
                }),
                Destination::Launcher { .. } => {
                    // This slot, then the ones below (scenes made as needed).
                    let i = match scene_index {
                        Some(i) => i,
                        None => break,
                    };
                    while i >= scenes.len() {
                        let id: SceneId = self.project.ids.allocate();
                        scenes.push(faderframe_project::launcher::Scene {
                            id,
                            name: format!("Scene {}", scenes.len() + 1),
                        });
                    }
                    let scene = scenes[i].id;
                    let key = faderframe_project::launcher::SlotKey { track, scene };
                    commands.push(Command::SetLauncherSlot {
                        track,
                        scene,
                        clip: Some(Box::new(clip)),
                    });
                    if let Some(bpm) = fit.and_then(|f| f.bpm) {
                        commands.push(Command::SetClipLaunch {
                            track,
                            scene,
                            launch: Some(faderframe_project::launcher::ClipLaunch {
                                tempo: Some(bpm),
                                ..self.project.launcher.launch_of(key)
                            }),
                        });
                    }
                    scene_index = Some(i + 1);
                }
            }
        }
        if launcher && scenes != self.project.launcher.scenes {
            commands.insert(0, Command::SetScenes { scenes });
        }
        let placed = clips.len();
        if !commands.is_empty() {
            self.edit(Command::Batch {
                label: if launcher {
                    "Browser to Launcher".into()
                } else if placed == 1 {
                    "Place from Browser".into()
                } else {
                    "Place Files from Browser".into()
                },
                commands,
            })?;
            if !launcher {
                self.selection
                    .select_clips(&clips, crate::SelectMode::Replace);
            }
        }
        for f in failures {
            self.notify(NoticeLevel::Error, format!("placing: {f}"));
        }
        Ok(())
    }
}

fn cmp_opt(a: Option<f64>, b: Option<f64>) -> std::cmp::Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.total_cmp(&b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

fn search_words(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_lowercase).collect()
}

/// Every word in the file's path below the folder looked in (its name
/// elsewhere), or in a tag.
fn matches_words(words: &[String], path: &Path, base: Option<&Path>, user: &UserData) -> bool {
    if words.is_empty() {
        return true;
    }
    let shown = base
        .and_then(|b| path.strip_prefix(b).ok())
        .unwrap_or_else(|| Path::new(path.file_name().unwrap_or(path.as_os_str())));
    let name = shown.to_string_lossy().to_lowercase();
    let tags: Vec<String> = user.tags_of(path).map(|t| t.to_lowercase()).collect();
    words
        .iter()
        .all(|w| name.contains(w.as_str()) || tags.iter().any(|t| t.contains(w.as_str())))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn key(text: &str) -> Key {
        Key::parse(text).unwrap()
    }

    #[test]
    fn keys_are_matched_by_their_notes() {
        assert_eq!(transposition(key("A minor"), key("C major")), 0);
        assert_eq!(transposition(key("C major"), key("D major")), 2);
        assert_eq!(transposition(key("C major"), key("Bb major")), -2);
        assert_eq!(transposition(key("E minor"), key("C minor")), -4);
        assert_eq!(transposition(key("C major"), key("F# major")), 6);
        assert!(keys_fit(key("C major"), key("G major")));
        assert!(keys_fit(key("D minor"), key("F major")));
        assert!(!keys_fit(key("C major"), key("E major")));
    }

    #[test]
    fn loops_and_tempos() {
        // Two bars at 120 BPM.
        assert!(is_loop(Some(120.0), Some(4.0)));
        assert!(is_loop(Some(120.0), Some(4.05)));
        assert!(!is_loop(Some(120.0), Some(1.75)));
        assert!(!is_loop(None, Some(4.0)));
        assert!(!is_loop(Some(120.0), Some(0.5)));
        assert_eq!(nearest_tempo(170.0, 90.0), 85.0);
        assert_eq!(nearest_tempo(60.0, 120.0), 120.0);
        assert_eq!(nearest_tempo(100.0, 104.0), 100.0);
        let fit = Fit {
            bpm: Some(100.0),
            project_bpm: 125.0,
            semitones: -2,
        };
        assert_eq!(fit.speed(), 1.25);
        assert_eq!(fit.text(), "100.0 → 125.0 BPM, -2 st");
    }

    #[test]
    fn payloads_carry_paths() {
        let files = vec![PathBuf::from("/a/kick.wav"), PathBuf::from("/b/c d.mid")];
        let p = payload(&files);
        assert!(p.starts_with(PAYLOAD));
        assert_eq!(parse_payload(&p), Some(files));
        assert_eq!(parse_payload("clips:1,2"), None);
        assert_eq!(parse_payload(PAYLOAD), None);
    }
}
