//! Samples for the sampler and the drum sampler: what a device's state
//! says to load (a list of files), loading them (any format the importer
//! reads, at their own rate — playback steps through them at the ratio),
//! SFZ instruments, and handing the result to the audio thread.
//!
//! The loaded set is immutable and shared: the processor reads it through
//! a [`TryCell`] (it only ever tries the lock, and plays nothing new for
//! the block it misses), the editor through the tap's assets. Replacing a
//! set happens on the control thread under the lock; the old one is
//! dropped there, never on the audio thread.

use faderframe_realtime::TryCell;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// What to load: per slot a file (the sampler has one, an audio file or an
/// SFZ; the drum sampler one per pad).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SampleDoc {
    #[serde(default)]
    pub files: Vec<Option<String>>,
}

impl SampleDoc {
    pub fn file(&self, slot: usize) -> Option<&str> {
        self.files.get(slot).and_then(|f| f.as_deref())
    }

    pub fn set(&mut self, slot: usize, file: Option<String>) {
        if self.files.len() <= slot {
            self.files.resize(slot + 1, None);
        }
        self.files[slot] = file;
        while self.files.last().is_some_and(Option::is_none) {
            self.files.pop();
        }
    }
}

/// A device state with samples: `FFSD`, the parameter block's length, the
/// parameter block, the document as JSON.
const MAGIC: &[u8; 4] = b"FFSD";

pub fn pack(params: &[u8], doc: &SampleDoc) -> Vec<u8> {
    let json = serde_json::to_vec(doc).unwrap_or_default();
    let mut out = Vec::with_capacity(8 + params.len() + json.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(params.len() as u32).to_le_bytes());
    out.extend_from_slice(params);
    out.extend_from_slice(&json);
    out
}

/// The parameter block and the document of a packed state (`None` for a
/// plain parameter block).
pub fn unpack(data: &[u8]) -> Option<(&[u8], SampleDoc)> {
    let rest = data.strip_prefix(MAGIC.as_slice())?;
    let (len, rest) = rest.split_first_chunk::<4>()?;
    let len = u32::from_le_bytes(*len) as usize;
    let (params, json) = (rest.get(..len)?, rest.get(len..)?);
    let doc = serde_json::from_slice(json).unwrap_or_default();
    Some((params, doc))
}

/// A decoded sample (one or two channels at its own rate).
#[derive(Debug)]
pub struct Sample {
    pub name: String,
    pub rate: f64,
    pub frames: usize,
    pub data: [Vec<f32>; 2],
    pub stereo: bool,
    /// Its loudest sample.
    pub peak: f32,
    /// Minimum and maximum (both channels) per stretch of frames, at most
    /// a few thousand of them, for drawing.
    pub overview: Vec<(f32, f32)>,
}

/// The most points an overview has.
const OVERVIEW_POINTS: usize = 4096;

impl Sample {
    pub fn from_channels(name: String, rate: f64, channels: Vec<Vec<f32>>) -> Self {
        let frames = channels.iter().map(Vec::len).min().unwrap_or(0);
        let stereo = channels.len() > 1;
        let mut it = channels.into_iter();
        let mut left = it.next().unwrap_or_default();
        left.truncate(frames);
        let right = match it.next() {
            Some(mut r) => {
                r.truncate(frames);
                r
            }
            None => left.clone(),
        };
        let step = frames.div_ceil(OVERVIEW_POINTS).max(1);
        let overview = left
            .chunks(step)
            .zip(right.chunks(step))
            .map(|(a, b)| {
                a.iter()
                    .chain(b)
                    .fold((0.0f32, 0.0f32), |(lo, hi), v| (lo.min(*v), hi.max(*v)))
            })
            .collect();
        let peak = left
            .iter()
            .chain(&right)
            .fold(0.0f32, |m, v| m.max(v.abs()));
        Self {
            name,
            rate,
            frames,
            data: [left, right],
            stereo,
            peak,
            overview,
        }
    }
}

/// The longest file loaded (frames at its rate): ten minutes at 48 kHz.
const MAX_FRAMES: usize = 48_000 * 600;

/// Decode an audio file.
pub fn load_audio(path: &Path) -> Result<Sample, String> {
    decode(path, |cancel, on_info, sink| {
        faderframe_audio_files::decode::decode_file(path, cancel, on_info, sink)
    })
}

/// Audio held in memory (an SFZ's embedded sample), `name` giving its
/// format by extension.
pub fn load_audio_bytes(name: &str, bytes: Vec<u8>) -> Result<Sample, String> {
    let path = Path::new(name);
    decode(path, |cancel, on_info, sink| {
        faderframe_audio_files::decode::decode_bytes(bytes, path, cancel, on_info, sink)
    })
}

type InfoFn<'a> = &'a mut dyn FnMut(
    &faderframe_audio_files::decode::ProbeInfo,
) -> Result<(), faderframe_audio_files::import::ImportError>;
type SinkFn<'a> = &'a mut dyn FnMut(
    &[Vec<f32>],
    usize,
) -> Result<(), faderframe_audio_files::import::ImportError>;

fn decode(
    path: &Path,
    run: impl FnOnce(
        &AtomicBool,
        InfoFn<'_>,
        SinkFn<'_>,
    ) -> Result<
        faderframe_audio_files::decode::ProbeInfo,
        faderframe_audio_files::import::ImportError,
    >,
) -> Result<Sample, String> {
    let cancel = AtomicBool::new(false);
    let mut channels: Vec<Vec<f32>> = Vec::new();
    let info = run(&cancel, &mut |_| Ok(()), &mut |block, frames| {
        if channels.is_empty() {
            channels = vec![Vec::new(); block.len().clamp(1, 2)];
        }
        for (c, ch) in channels.iter_mut().enumerate() {
            if ch.len() < MAX_FRAMES {
                let src = &block[c.min(block.len() - 1)];
                ch.extend_from_slice(&src[..frames.min(src.len())]);
            }
        }
        Ok(())
    })
    .map_err(|e| format!("{}: {e}", path.display()))?;
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(Sample::from_channels(
        name,
        f64::from(info.sample_rate.max(1)),
        channels,
    ))
}

/// Decoded samples by file, while anyone holds them (and the file has
/// not changed since): a preload on a worker thread makes the instance's
/// own load instant, and offline renders share the live one's samples.
/// Each file's decoded sample (while held) and when the file changed.
type Cache = std::sync::Mutex<
    std::collections::HashMap<PathBuf, (Option<std::time::SystemTime>, std::sync::Weak<Sample>)>,
>;

fn cache() -> &'static Cache {
    static CACHE: std::sync::OnceLock<Cache> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Decode an audio file, or share it if it is decoded already.
pub fn load_cached(path: &Path) -> Result<Arc<Sample>, String> {
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    if let Ok(c) = cache().lock()
        && let Some((when, weak)) = c.get(path)
        && *when == modified
        && let Some(s) = weak.upgrade()
    {
        return Ok(s);
    }
    let s = Arc::new(load_audio(path)?);
    if let Ok(mut c) = cache().lock() {
        c.retain(|_, (_, w)| w.strong_count() > 0);
        c.insert(path.to_path_buf(), (modified, Arc::downgrade(&s)));
    }
    Ok(s)
}

/// A packed state with every file of its document mapped by `f` (`None`
/// for a plain parameter block).
pub fn map_paths(state: &[u8], f: impl Fn(&Path) -> PathBuf) -> Option<Vec<u8>> {
    let (params, mut doc) = unpack(state)?;
    for file in doc.files.iter_mut().flatten() {
        *file = f(Path::new(file.as_str())).to_string_lossy().into_owned();
    }
    Some(pack(params, &doc))
}

/// Whether a file is an SFZ instrument (referenced where it lies, with its
/// samples, rather than copied).
pub fn is_sfz(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("sfz"))
}

/// How a zone loops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopMode {
    /// Plays to its end (or the release).
    NoLoop,
    /// Plays to its end whatever the key does.
    OneShot,
    /// Loops for ever (the release fades it).
    Continuous,
    /// Loops while the key is held, then plays on to the end.
    Sustain,
}

pub use super::sfz::{Zone, parse_key};

/// What a device plays: its samples and (for an instrument) the zones
/// mapping them, with what could not be loaded.
#[derive(Debug, Default)]
pub struct SampleSet {
    /// Per document slot, the sample loaded there (drums: one per pad).
    pub slots: Vec<Option<usize>>,
    pub samples: Vec<Arc<Sample>>,
    /// Instrument zones (an SFZ's regions); empty when a slot plays its
    /// sample by the device's own settings.
    pub zones: Vec<Zone>,
    /// An SFZ's curves and controllers' initial values.
    pub instrument: Option<Arc<super::sfz::Instrument>>,
    pub errors: Vec<String>,
}

impl SampleSet {
    pub fn slot(&self, slot: usize) -> Option<&Sample> {
        self.slots
            .get(slot)
            .copied()
            .flatten()
            .map(|i| self.samples[i].as_ref())
    }
}

/// The set a processor plays and its generation (voices of another
/// generation stop).
#[derive(Debug, Default)]
pub struct Loaded {
    pub generation: u64,
    pub set: Option<Arc<SampleSet>>,
}

pub type Shared = Arc<TryCell<Loaded>>;

/// A shared slot with nothing loaded.
pub fn empty() -> Shared {
    Arc::new(TryCell::new(Loaded::default()))
}

/// What a sampler's editor shows: the document and what loaded from it.
#[derive(Debug)]
pub struct Contents {
    pub doc: SampleDoc,
    pub set: Arc<SampleSet>,
}

/// An instance's side of its samples: the document, the set shared with
/// the processor, and a set waiting to be swapped in.
pub struct SampleHost {
    pub doc: SampleDoc,
    pub shared: Shared,
    generation: u64,
    pending: Option<Arc<SampleSet>>,
}

impl Default for SampleHost {
    fn default() -> Self {
        Self {
            doc: SampleDoc::default(),
            shared: empty(),
            generation: 0,
            pending: None,
        }
    }
}

impl SampleHost {
    /// Load what `doc` names (unless it is what is loaded) and hand it to
    /// the processor and the editor.
    pub fn set_doc(&mut self, doc: SampleDoc, tap: Option<&crate::tap::AnalysisTap>) {
        if doc == self.doc && self.generation > 0 {
            return;
        }
        let set = Arc::new(load(&doc));
        self.doc = doc;
        if let Some(t) = tap {
            t.set_assets(Arc::new(Contents {
                doc: self.doc.clone(),
                set: Arc::clone(&set),
            }));
        }
        self.pending = Some(set);
        self.flush();
    }

    /// Swap a waiting set in (the old one is dropped here, on the control
    /// thread).
    pub fn flush(&mut self) {
        let Some(set) = self.pending.take() else {
            return;
        };
        let old = match self.shared.lock_blocking(400) {
            Some(mut g) => {
                self.generation += 1;
                std::mem::replace(
                    &mut *g,
                    Loaded {
                        generation: self.generation,
                        set: Some(set),
                    },
                )
            }
            None => {
                self.pending = Some(set);
                return;
            }
        };
        drop(old);
    }
}

/// Load a document: SFZ files become zones, other files plain samples.
pub fn load(doc: &SampleDoc) -> SampleSet {
    let mut set = SampleSet::default();
    for file in &doc.files {
        let Some(file) = file else {
            set.slots.push(None);
            continue;
        };
        let path = PathBuf::from(file);
        if is_sfz(&path) {
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    super::sfz::load(&mut set, &text, &path);
                    set.slots.push(None);
                }
                Err(e) => {
                    set.errors.push(format!("{}: {e}", path.display()));
                    set.slots.push(None);
                }
            }
            continue;
        }
        match load_cached(&path) {
            Ok(s) => {
                set.slots.push(Some(set.samples.len()));
                set.samples.push(s);
            }
            Err(e) => {
                set.errors.push(e);
                set.slots.push(None);
            }
        }
    }
    set
}

/// Test fixtures: a folder of tones.
#[cfg(test)]
pub(crate) mod fixtures {
    use std::path::{Path, PathBuf};

    /// A fresh folder for a test.
    pub fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ff-samples-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).ok();
        d
    }

    /// A sine of `freq` at `rate` for `seconds` (a short fade at its end).
    pub fn tone(path: &Path, freq: f64, rate: u32, seconds: f64) -> String {
        let n = (seconds * f64::from(rate)) as usize;
        let x: Vec<f32> = (0..n)
            .map(|i| {
                let fade = ((n - i) as f64 / 200.0).min(1.0);
                (0.5 * fade * (std::f64::consts::TAU * freq * i as f64 / f64::from(rate)).sin())
                    as f32
            })
            .collect();
        faderframe_audio_files::write_wav(
            path,
            &[x],
            rate,
            faderframe_audio_files::WavFormat::Float32,
            false,
        )
        .ok();
        path.to_string_lossy().into_owned()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn states_pack_and_unpack() {
        let mut doc = SampleDoc::default();
        doc.set(2, Some("/x/kick.wav".into()));
        assert_eq!(doc.files.len(), 3);
        let packed = pack(&[1, 2, 3], &doc);
        let (params, back) = unpack(&packed).unwrap();
        assert_eq!(params, &[1, 2, 3]);
        assert_eq!(back, doc);
        let moved = map_paths(&packed, |p| Path::new("/y").join(p.file_name().unwrap())).unwrap();
        let want = Path::new("/y").join("kick.wav");
        assert_eq!(unpack(&moved).unwrap().1.file(2), want.to_str());
        assert_eq!(unpack(&moved).unwrap().0, &[1, 2, 3]);
        assert!(unpack(&[0; 12]).is_none(), "a plain parameter block");
        doc.set(2, None);
        assert!(doc.files.is_empty());
    }

    #[test]
    fn keys_and_sfz_headers() {
        assert_eq!(parse_key("c4"), Some(60));
        assert_eq!(parse_key("A#3"), Some(58));
        assert_eq!(parse_key("eb2"), Some(39));
        assert_eq!(parse_key("61"), Some(61));
        let h = crate::devices::sfz::headers(
            "// a piano\n<control> default_path=Samples\\\n<group> ampeg_release=0.4 lovel=0\n<region> sample=Piano C4 v1.wav key=c4\n<region>sample=d4.wav lokey=d4 hikey=e4 pitch_keycenter=d4 tune=-5",
        );
        assert_eq!(h.len(), 4);
        assert_eq!(
            h[2].1,
            vec![
                ("sample".to_string(), "Piano C4 v1.wav".to_string()),
                ("key".to_string(), "c4".to_string())
            ]
        );
        assert_eq!(h[3].1[1], ("lokey".to_string(), "d4".to_string()));
        assert_eq!(h[3].1[4], ("tune".to_string(), "-5".to_string()));
    }

    #[test]
    fn an_sfz_instrument_loads_its_regions() {
        let dir = std::env::temp_dir().join(format!("ff-sfz-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("s")).unwrap();
        let tone: Vec<f32> = (0..4_800).map(|n| (n as f32 * 0.05).sin() * 0.5).collect();
        for name in ["low", "high"] {
            faderframe_audio_files::write_wav(
                &dir.join("s").join(format!("{name}.wav")),
                std::slice::from_ref(&tone),
                44_100,
                faderframe_audio_files::WavFormat::Float32,
                false,
            )
            .unwrap();
        }
        let sfz = "<control>default_path=s/\n<global>ampeg_release=0.3\n<group>loop_mode=loop_continuous loop_start=100 loop_end=4000\n<region>sample=low.wav lokey=c2 hikey=b3 pitch_keycenter=c3\n<region>sample=high.wav lokey=c4 hikey=c6 pitch_keycenter=60 seq_length=2 seq_position=2\n<region>sample=missing.wav key=10";
        std::fs::write(dir.join("i.sfz"), sfz).unwrap();
        let mut doc = SampleDoc::default();
        doc.set(0, Some(dir.join("i.sfz").to_string_lossy().into_owned()));
        let set = load(&doc);
        assert_eq!(set.samples.len(), 2);
        assert_eq!(set.zones.len(), 2);
        assert_eq!(set.errors.len(), 1, "{:?}", set.errors);
        let z = &set.zones[0];
        assert_eq!((z.lokey, z.hikey, z.root), (36, 59, 48.0));
        assert_eq!(z.loop_mode, Some(LoopMode::Continuous));
        assert_eq!((z.loop_start, z.loop_end), (Some(100), Some(4000)));
        assert_eq!(z.ampeg.release, 0.3);
        assert_eq!(set.zones[1].seq_length, 2);
        assert_eq!(set.samples[0].rate, 44_100.0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
