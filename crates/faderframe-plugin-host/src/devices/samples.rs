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
    let cancel = AtomicBool::new(false);
    let mut channels: Vec<Vec<f32>> = Vec::new();
    let info = faderframe_audio_files::decode::decode_file(
        path,
        &cancel,
        |_| Ok(()),
        |block, frames| {
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
        },
    )
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

/// One region of an instrument: a sample over a range of keys and
/// velocities, with its tuning, level, loop and envelope.
#[derive(Clone, Debug, PartialEq)]
pub struct Zone {
    pub sample: usize,
    pub lokey: u8,
    pub hikey: u8,
    pub lovel: u8,
    pub hivel: u8,
    pub root: f64,
    /// Cents.
    pub tune: f64,
    pub volume: f64,
    pub pan: f64,
    pub loop_mode: Option<LoopMode>,
    pub loop_start: Option<usize>,
    pub loop_end: Option<usize>,
    pub offset: usize,
    pub end: Option<usize>,
    /// Attack, decay, sustain (0–1), release in seconds, where the file
    /// says.
    pub ampeg: [Option<f64>; 4],
    pub on_release: bool,
    pub group: u32,
    pub off_by: u32,
    pub seq_length: u32,
    pub seq_position: u32,
    pub lorand: f64,
    pub hirand: f64,
    /// Velocity tracking (0–1) and key tracking (cents per key).
    pub veltrack: f64,
    pub keytrack: f64,
}

impl Zone {
    pub fn whole(sample: usize) -> Self {
        Self {
            sample,
            lokey: 0,
            hikey: 127,
            lovel: 0,
            hivel: 127,
            root: 60.0,
            tune: 0.0,
            volume: 0.0,
            pan: 0.0,
            loop_mode: None,
            loop_start: None,
            loop_end: None,
            offset: 0,
            end: None,
            ampeg: [None; 4],
            on_release: false,
            group: 0,
            off_by: 0,
            seq_length: 1,
            seq_position: 1,
            lorand: 0.0,
            hirand: 1.0,
            veltrack: 1.0,
            keytrack: 100.0,
        }
    }

    pub fn takes(&self, key: u8, velocity: u8) -> bool {
        (self.lokey..=self.hikey).contains(&key) && (self.lovel..=self.hivel).contains(&velocity)
    }
}

/// What a device plays: its samples and (for an instrument) the zones
/// mapping them, with what could not be loaded.
#[derive(Debug, Default)]
pub struct SampleSet {
    /// Per document slot, the sample loaded there (drums: one per pad).
    pub slots: Vec<Option<usize>>,
    pub samples: Vec<Sample>,
    /// Instrument zones (an SFZ's regions); empty when a slot plays its
    /// sample by the device's own settings.
    pub zones: Vec<Zone>,
    pub errors: Vec<String>,
}

impl SampleSet {
    pub fn slot(&self, slot: usize) -> Option<&Sample> {
        self.slots
            .get(slot)
            .copied()
            .flatten()
            .map(|i| &self.samples[i])
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
        let sfz = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("sfz"));
        if sfz {
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    let base = path.parent().map(Path::to_path_buf).unwrap_or_default();
                    load_sfz(&mut set, &text, &base);
                    set.slots.push(None);
                }
                Err(e) => {
                    set.errors.push(format!("{}: {e}", path.display()));
                    set.slots.push(None);
                }
            }
            continue;
        }
        match load_audio(&path) {
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

/// A note name or number (`c4` = 60, `a#3`, `eb2`, `61`).
pub fn parse_key(v: &str) -> Option<u8> {
    let v = v.trim();
    if let Ok(n) = v.parse::<i32>() {
        return u8::try_from(n.clamp(0, 127)).ok();
    }
    let lower = v.to_ascii_lowercase();
    let mut chars = lower.chars();
    let base = match chars.next()? {
        'c' => 0,
        'd' => 2,
        'e' => 4,
        'f' => 5,
        'g' => 7,
        'a' => 9,
        'b' => 11,
        _ => return None,
    };
    let rest: String = chars.collect();
    let (shift, octave) = if let Some(o) = rest.strip_prefix('#') {
        (1, o)
    } else if let Some(o) = rest.strip_prefix('b') {
        (-1, o)
    } else {
        (0, rest.as_str())
    };
    let octave: i32 = octave.parse().ok()?;
    u8::try_from((12 * (octave + 1) + base + shift).clamp(0, 127)).ok()
}

/// The opcodes of one header, as written.
type Opcodes = Vec<(String, String)>;

/// Split SFZ text into headers and their opcodes (comments removed).
fn sfz_headers(text: &str) -> Vec<(String, Opcodes)> {
    let mut out: Vec<(String, Opcodes)> = Vec::new();
    for raw in text.lines() {
        let line = match raw.find("//") {
            Some(i) => &raw[..i],
            None => raw,
        };
        let mut rest = line.trim();
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix('<') {
                let Some(end) = after.find('>') else { break };
                out.push((after[..end].trim().to_ascii_lowercase(), Vec::new()));
                rest = after[end + 1..].trim_start();
                continue;
            }
            let Some(eq) = rest.find('=') else { break };
            let key = rest[..eq].trim().to_ascii_lowercase();
            let after = &rest[eq + 1..];
            // The value (spaces and all: sample paths have them) runs to
            // the next opcode (`name=`) or header.
            let end = {
                let mut end = after.len();
                let bytes = after.as_bytes();
                let mut i = 0;
                while i < bytes.len() {
                    if bytes[i] == b'<' {
                        end = i;
                        break;
                    }
                    if bytes[i].is_ascii_whitespace() {
                        let tail = after[i..].trim_start();
                        let word_end =
                            tail.find(|c: char| c == '=' || c.is_whitespace() || c == '<');
                        if let Some(w) = word_end
                            && tail[w..].starts_with('=')
                        {
                            end = i;
                            break;
                        }
                    }
                    i += 1;
                }
                end
            };
            let value = after[..end].trim().to_string();
            if out.is_empty() {
                out.push(("global".into(), Vec::new()));
            }
            if let Some(last) = out.last_mut() {
                last.1.push((key, value));
            }
            rest = after[end..].trim_start();
        }
    }
    out
}

/// Read an SFZ instrument into `set` (its samples relative to `base`, or
/// to `default_path`).
pub fn load_sfz(set: &mut SampleSet, text: &str, base: &Path) {
    let headers = sfz_headers(text);
    let mut global: Opcodes = Vec::new();
    let mut master: Opcodes = Vec::new();
    let mut group: Opcodes = Vec::new();
    let mut default_path = String::new();
    let mut loaded: std::collections::HashMap<PathBuf, Option<usize>> = Default::default();
    for (header, ops) in headers {
        match header.as_str() {
            "control" => {
                for (k, v) in &ops {
                    if k == "default_path" {
                        default_path = v.replace('\\', "/");
                    }
                }
            }
            "global" => {
                global = ops;
                master.clear();
                group.clear();
            }
            "master" => {
                master = ops;
                group.clear();
            }
            "group" => group = ops,
            "region" => {
                let all: Vec<&(String, String)> = global
                    .iter()
                    .chain(&master)
                    .chain(&group)
                    .chain(&ops)
                    .collect();
                let Some(sample) = all
                    .iter()
                    .rev()
                    .find(|(k, _)| k == "sample")
                    .map(|(_, v)| v.replace('\\', "/"))
                else {
                    continue;
                };
                let path = base.join(&default_path).join(&sample);
                let index =
                    *loaded
                        .entry(path.clone())
                        .or_insert_with(|| match load_audio(&path) {
                            Ok(s) => {
                                set.samples.push(s);
                                Some(set.samples.len() - 1)
                            }
                            Err(e) => {
                                set.errors.push(e);
                                None
                            }
                        });
                let Some(index) = index else { continue };
                let mut z = Zone::whole(index);
                let mut key_set = false;
                for (k, v) in all {
                    let f = || v.parse::<f64>().ok();
                    match k.as_str() {
                        "lokey" => z.lokey = parse_key(v).unwrap_or(z.lokey),
                        "hikey" => z.hikey = parse_key(v).unwrap_or(z.hikey),
                        "key" => {
                            if let Some(n) = parse_key(v) {
                                (z.lokey, z.hikey, z.root) = (n, n, f64::from(n));
                                key_set = true;
                            }
                        }
                        "pitch_keycenter" => {
                            if let Some(n) = parse_key(v) {
                                z.root = f64::from(n);
                                key_set = true;
                            }
                        }
                        "lovel" => z.lovel = v.parse().unwrap_or(z.lovel),
                        "hivel" => z.hivel = v.parse().unwrap_or(z.hivel),
                        "tune" => z.tune = f().unwrap_or(0.0),
                        "transpose" => z.tune += 100.0 * f().unwrap_or(0.0),
                        "volume" => z.volume = f().unwrap_or(0.0),
                        "pan" => z.pan = (f().unwrap_or(0.0) / 100.0).clamp(-1.0, 1.0),
                        "loop_mode" | "loopmode" => {
                            z.loop_mode = match v.as_str() {
                                "no_loop" => Some(LoopMode::NoLoop),
                                "one_shot" => Some(LoopMode::OneShot),
                                "loop_continuous" => Some(LoopMode::Continuous),
                                "loop_sustain" => Some(LoopMode::Sustain),
                                _ => z.loop_mode,
                            }
                        }
                        "loop_start" | "loopstart" => z.loop_start = v.parse().ok(),
                        "loop_end" | "loopend" => z.loop_end = v.parse().ok(),
                        "offset" => z.offset = v.parse().unwrap_or(0),
                        "end" => z.end = v.parse().ok(),
                        "ampeg_attack" => z.ampeg[0] = f(),
                        "ampeg_decay" => z.ampeg[1] = f(),
                        "ampeg_sustain" => z.ampeg[2] = f().map(|s| s / 100.0),
                        "ampeg_release" => z.ampeg[3] = f(),
                        "trigger" => z.on_release = v == "release",
                        "group" => z.group = v.parse().unwrap_or(0),
                        "off_by" => z.off_by = v.parse().unwrap_or(0),
                        "seq_length" => z.seq_length = v.parse::<u32>().unwrap_or(1).max(1),
                        "seq_position" => z.seq_position = v.parse::<u32>().unwrap_or(1).max(1),
                        "lorand" => z.lorand = f().unwrap_or(0.0),
                        "hirand" => z.hirand = f().unwrap_or(1.0),
                        "amp_veltrack" => z.veltrack = f().unwrap_or(100.0) / 100.0,
                        "pitch_keytrack" => z.keytrack = f().unwrap_or(100.0),
                        _ => {}
                    }
                }
                if !key_set && z.lokey == z.hikey {
                    z.root = f64::from(z.lokey);
                }
                set.zones.push(z);
            }
            _ => {}
        }
    }
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
        let h = sfz_headers(
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
        assert_eq!(z.ampeg[3], Some(0.3));
        assert_eq!(set.zones[1].seq_length, 2);
        assert_eq!(set.samples[0].rate, 44_100.0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
