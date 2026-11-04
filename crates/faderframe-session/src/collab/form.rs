//! Steps and projects as they are sent: MessagePack, with every media path
//! replaced by a token naming the file by its content.
//!
//! Each session keeps its media where it likes (absolute paths in memory,
//! as everywhere in the session), so a path means nothing on another
//! machine. On the way out every string that is an absolute path of a
//! regular file becomes `ff-media:<key>/<file name>` — the key is the
//! file's length and SHA-256 (`<len>:<hex>`) — and so do the files named
//! inside samplers' states (`FFSD` blobs). On the way in each token
//! becomes the path of the file with that key here, or of the place it
//! is fetched to. Walking the serialised value finds paths wherever a
//! command carries them (sources, video, samplers, a whole project in a
//! merge), so new commands need nothing done for them.

use faderframe_plugin_host::devices::samples;
use rmpv::Value;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::SystemTime;

/// What a media token starts with.
pub(crate) const TOKEN: &str = "ff-media:";

#[derive(Debug, thiserror::Error)]
pub enum FormError {
    #[error("could not write it: {0}")]
    Encode(String),
    #[error("could not read it: {0}")]
    Decode(String),
    /// Files it names are still being hashed: try again soon.
    #[error("{0} media files are still being hashed")]
    Hashing(usize),
}

/// What is known of a path's content key.
pub(crate) enum Lookup {
    Known(String),
    /// Being hashed in the background.
    Hashing,
    /// No regular file.
    NotAFile,
}

type Hashed = (PathBuf, u64, Option<SystemTime>, Option<String>);

/// Content keys of files here (`<length>:<sha-256 hex>`), remembered by
/// path while a file keeps its length and time. Files are hashed on a
/// thread of their own (a long recording takes a while): what names a
/// file not hashed yet waits ([`FormError::Hashing`]).
#[derive(Default)]
pub(crate) struct Keys {
    hashed: HashMap<PathBuf, (u64, Option<SystemTime>, String)>,
    /// Places files are fetched to (they are not here yet).
    coming: HashMap<PathBuf, String>,
    asked: std::collections::HashSet<PathBuf>,
    worker: Option<(Sender<PathBuf>, Receiver<Hashed>)>,
    /// Hash on the calling thread (tests).
    inline: bool,
}

impl Keys {
    /// Keys that hash on the calling thread.
    pub fn inline() -> Self {
        Self {
            inline: true,
            ..Self::default()
        }
    }

    /// Take in what the hashing thread finished.
    pub fn poll(&mut self) {
        let Some((_, rx)) = &self.worker else {
            return;
        };
        let done: Vec<Hashed> = rx.try_iter().collect();
        for (path, len, time, key) in done {
            self.asked.remove(&path);
            if let Some(k) = key {
                self.hashed.insert(path, (len, time, k));
            }
        }
    }

    /// Files asked for and not hashed yet.
    pub fn hashing(&self) -> usize {
        self.asked.len()
    }

    /// The key of the regular file at `path`.
    pub fn lookup(&mut self, path: &Path) -> Lookup {
        if let Some(k) = self.coming.get(path) {
            return Lookup::Known(k.clone());
        }
        let Some(meta) = std::fs::metadata(path).ok().filter(|m| m.is_file()) else {
            return Lookup::NotAFile;
        };
        let (len, time) = (meta.len(), meta.modified().ok());
        if let Some((l, t, k)) = self.hashed.get(path)
            && *l == len
            && *t == time
        {
            return Lookup::Known(k.clone());
        }
        if self.inline {
            return match hash_file(path) {
                Some(k) => {
                    self.hashed
                        .insert(path.to_path_buf(), (len, time, k.clone()));
                    Lookup::Known(k)
                }
                None => Lookup::NotAFile,
            };
        }
        if self.asked.insert(path.to_path_buf()) {
            let tx = &self.worker.get_or_insert_with(spawn_hasher).0;
            if tx.send(path.to_path_buf()).is_err() {
                self.asked.remove(path);
                self.worker = None;
            }
        }
        Lookup::Hashing
    }

    /// The key, when it is known now.
    pub fn key_of(&mut self, path: &Path) -> Option<String> {
        match self.lookup(path) {
            Lookup::Known(k) => Some(k),
            _ => None,
        }
    }

    /// A file with `key` will be at `path`.
    pub fn coming(&mut self, path: &Path, key: &str) {
        self.coming.insert(path.to_path_buf(), key.to_string());
    }

    /// It arrived (its content is the key's: no need to hash it), or will
    /// not.
    pub fn arrived(&mut self, path: &Path) {
        if let Some(k) = self.coming.remove(path)
            && let Ok(meta) = std::fs::metadata(path)
        {
            self.hashed
                .insert(path.to_path_buf(), (meta.len(), meta.modified().ok(), k));
        }
    }

    /// Forget the place a fetch that will not happen was going to.
    pub fn not_coming(&mut self, path: &Path) {
        self.coming.remove(path);
    }
}

fn hash_file(path: &Path) -> Option<String> {
    let len = std::fs::metadata(path).ok()?.len();
    let sha = faderframe_collab::sha256_of(path).ok()?;
    Some(format!("{len}:{}", hex(&sha)))
}

fn spawn_hasher() -> (Sender<PathBuf>, Receiver<Hashed>) {
    let (tx, jobs) = mpsc::channel::<PathBuf>();
    let (done, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("ff-collab-hash".into())
        .spawn(move || {
            faderframe_realtime::priority::lower_this_thread();
            for path in jobs {
                let meta = std::fs::metadata(&path).ok();
                let key = hash_file(&path);
                let (len, time) = meta.map_or((0, None), |m| (m.len(), m.modified().ok()));
                if done.send((path, len, time, key)).is_err() {
                    return;
                }
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("collaboration: no hashing thread: {e}");
    }
    (tx, rx)
}

pub(crate) fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The length a key names.
pub(crate) fn key_length(key: &str) -> Option<u64> {
    key.split_once(':')?.0.parse().ok()
}

/// How tokens are written.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Naming {
    /// Key and file name (what is sent: the name says where it goes).
    Named,
    /// The key alone (what is compared: names differ between sessions).
    KeyOnly,
}

fn token(key: &str, path: &Path, naming: Naming) -> String {
    match naming {
        Naming::Named => format!(
            "{TOKEN}{key}/{}",
            path.file_name()
                .map_or_else(|| "media".into(), |n| n.to_string_lossy())
        ),
        Naming::KeyOnly => format!("{TOKEN}{key}"),
    }
}

/// `(key, file name)` of a token.
fn parse_token(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix(TOKEN)?;
    Some(rest.split_once('/').unwrap_or((rest, "media")))
}

/// Whether a string can be a sampler's state (`FFSD` in base 64).
fn sampler_state(text: &str) -> Option<Vec<u8>> {
    if !text.starts_with("RkZTR") {
        return None;
    }
    faderframe_engine::decode_state(text).filter(|b| b.starts_with(b"FFSD"))
}

fn walk(v: &mut Value, f: &mut dyn FnMut(&str) -> Option<String>) {
    match v {
        Value::String(s) => {
            if let Some(new) = s.as_str().and_then(&mut *f) {
                *v = Value::from(new);
            }
        }
        Value::Array(a) => {
            for x in a {
                walk(x, f);
            }
        }
        Value::Map(m) => {
            for (_, x) in m {
                walk(x, f);
            }
        }
        _ => {}
    }
}

fn to_value<T: serde::Serialize>(value: &T) -> Result<Value, FormError> {
    let bytes = rmp_serde::to_vec_named(value).map_err(|e| FormError::Encode(e.to_string()))?;
    rmpv::decode::read_value(&mut bytes.as_slice()).map_err(|e| FormError::Encode(e.to_string()))
}

fn bytes_of(v: &Value) -> Result<Vec<u8>, FormError> {
    let mut out = Vec::new();
    rmpv::encode::write_value(&mut out, v).map_err(|e| FormError::Encode(e.to_string()))?;
    Ok(out)
}

/// A path's token (`None`: no file of ours, or not hashed yet — counted).
fn path_token(
    p: &Path,
    keys: &mut Keys,
    naming: Naming,
    named: &mut BTreeMap<String, PathBuf>,
    hashing: &mut usize,
) -> Option<String> {
    if !p.is_absolute() {
        return None;
    }
    match keys.lookup(p) {
        Lookup::Known(key) => {
            named.insert(key.clone(), p.to_path_buf());
            Some(token(&key, p, naming))
        }
        Lookup::Hashing => {
            *hashing += 1;
            None
        }
        Lookup::NotAFile => None,
    }
}

/// Replace media paths by tokens; the files named, by key, go to `named`.
/// Returns how many named files are still being hashed.
fn tokenise(
    v: &mut Value,
    keys: &mut Keys,
    naming: Naming,
    named: &mut BTreeMap<String, PathBuf>,
) -> usize {
    keys.poll();
    let mut hashing = 0;
    walk(v, &mut |text| {
        if let Some(bytes) = sampler_state(text) {
            let cell = RefCell::new((&mut *keys, &mut *named, &mut hashing));
            let mapped = samples::map_paths(&bytes, |p| {
                let (k, n, h) = &mut *cell.borrow_mut();
                path_token(p, k, naming, n, h).map_or_else(|| p.to_path_buf(), PathBuf::from)
            })?;
            return (mapped != bytes).then(|| faderframe_engine::encode_state(&mapped));
        }
        if text.starts_with(TOKEN) {
            return None;
        }
        path_token(Path::new(text), keys, naming, named, &mut hashing)
    });
    hashing
}

/// `value` as sent.
pub(crate) fn to_wire<T: serde::Serialize>(
    value: &T,
    keys: &mut Keys,
    named: &mut BTreeMap<String, PathBuf>,
) -> Result<Vec<u8>, FormError> {
    let mut v = to_value(value)?;
    match tokenise(&mut v, keys, Naming::Named, named) {
        0 => bytes_of(&v),
        n => Err(FormError::Hashing(n)),
    }
}

/// Read what was sent; each token's `(key, file name)` becomes a path
/// here by `resolve`.
pub(crate) fn from_wire<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
    resolve: &mut dyn FnMut(&str, &str) -> PathBuf,
) -> Result<T, FormError> {
    let mut v =
        rmpv::decode::read_value(&mut &bytes[..]).map_err(|e| FormError::Decode(e.to_string()))?;
    walk(&mut v, &mut |text| {
        if let Some(bytes) = sampler_state(text) {
            let cell = RefCell::new(&mut *resolve);
            let mapped = samples::map_paths(&bytes, |p| {
                let s = p.to_string_lossy();
                parse_token(&s).map_or_else(|| p.to_path_buf(), |(k, n)| (cell.borrow_mut())(k, n))
            })?;
            return (mapped != bytes).then(|| faderframe_engine::encode_state(&mapped));
        }
        let (k, n) = parse_token(text)?;
        Some(resolve(k, n).to_string_lossy().into_owned())
    });
    let b = bytes_of(&v)?;
    rmp_serde::from_slice(&b).map_err(|e| FormError::Decode(e.to_string()))
}

/// The SHA-256 of a project's canonical form: media by key alone, maps
/// in key order, the id allocator left out (each session hands out ids
/// of its own; see [`faderframe_core::IdLease`]). Two sessions with
/// alike projects get the same hash.
pub(crate) fn canonical_hash(
    project: &faderframe_project::Project,
    keys: &mut Keys,
) -> Result<[u8; 32], FormError> {
    let mut v = to_value(project)?;
    let mut named = BTreeMap::new();
    let hashing = tokenise(&mut v, keys, Naming::KeyOnly, &mut named);
    if hashing > 0 {
        return Err(FormError::Hashing(hashing));
    }
    if let Value::Map(m) = &mut v {
        m.retain(|(k, _)| k.as_str() != Some("ids"));
    }
    sort_maps(&mut v);
    Ok(faderframe_collab::sha256(&bytes_of(&v)?))
}

fn sort_maps(v: &mut Value) {
    match v {
        Value::Array(a) => a.iter_mut().for_each(sort_maps),
        Value::Map(m) => {
            for (_, x) in m.iter_mut() {
                sort_maps(x);
            }
            m.sort_by_cached_key(|(k, _)| bytes_of(k).unwrap_or_default());
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_project::{AudioSource, Command, Project, SourceSpec};

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ff-collab-form-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_demo_project_crosses_exactly() {
        let p = faderframe_project::demo::demo_project(48_000);
        let cmd = Command::ReplaceProject {
            project: Box::new(p),
            label: "Merge".into(),
        };
        let mut named = BTreeMap::new();
        let b = to_wire(&cmd, &mut Keys::inline(), &mut named).unwrap();
        let back: Command = from_wire(&b, &mut |_, _| PathBuf::new()).unwrap();
        assert_eq!(back, cmd);
        assert!(named.is_empty(), "the demo's sources are generated");
    }

    #[test]
    fn media_paths_cross_as_tokens_and_land_where_the_files_are_here() {
        let d = dir("tokens");
        let file = d.join("Kick.wav");
        std::fs::write(&file, b"RIFF....WAVEdata").unwrap();
        let mut p = Project::new("t", 48_000);
        let id = p.ids.allocate();
        let source = AudioSource {
            id,
            name: "Kick".into(),
            spec: SourceSpec::File {
                path: file.clone(),
                channels: 1,
                frames: 4,
                sample_rate: 48_000,
            },
        };
        let cmd = Command::AddSource {
            source: Box::new(source),
        };
        let mut keys = Keys::inline();
        let mut named = BTreeMap::new();
        let b = to_wire(&cmd, &mut keys, &mut named).unwrap();
        let key = keys.key_of(&file).unwrap();
        assert!(key.starts_with("16:"), "{key}");
        assert_eq!(named.get(&key), Some(&file));
        let text = String::from_utf8_lossy(&b);
        assert!(
            !text.contains(&*d.to_string_lossy()),
            "no path of here is sent"
        );
        let here = PathBuf::from("/elsewhere/Media/Kick.wav");
        let mut seen = Vec::new();
        let back: Command = from_wire(&b, &mut |k, n| {
            seen.push((k.to_string(), n.to_string()));
            here.clone()
        })
        .unwrap();
        assert_eq!(seen, vec![(key, "Kick.wav".to_string())]);
        let Command::AddSource { source } = back else {
            panic!()
        };
        assert!(matches!(&source.spec, SourceSpec::File { path, .. } if *path == here));
    }

    #[test]
    fn samplers_name_their_files_by_content_too() {
        let d = dir("samplers");
        let file = d.join("snare.wav");
        std::fs::write(&file, b"snare").unwrap();
        let doc = samples::SampleDoc {
            files: vec![Some(file.to_string_lossy().into_owned())],
            ..Default::default()
        };
        let state = faderframe_engine::encode_state(&samples::pack(&[1, 2, 3], &doc));
        let cmd = Command::SetPluginState {
            track: faderframe_core::TrackId(1),
            plugin: faderframe_core::PluginInstanceId(2),
            state: Some(state),
            parameters: Vec::new(),
        };
        let mut named = BTreeMap::new();
        let b = to_wire(&cmd, &mut Keys::inline(), &mut named).unwrap();
        assert_eq!(named.len(), 1);
        let back: Command = from_wire(&b, &mut |_, n| PathBuf::from("/here").join(n)).unwrap();
        let Command::SetPluginState {
            state: Some(state), ..
        } = back
        else {
            panic!()
        };
        let bytes = faderframe_engine::decode_state(&state).unwrap();
        let (params, doc) = samples::unpack(&bytes).unwrap();
        assert_eq!(params, &[1, 2, 3]);
        assert_eq!(doc.files, vec![Some("/here/snare.wav".to_string())]);
    }

    #[test]
    fn alike_projects_hash_alike_wherever_their_media_are() {
        let (a, b) = (dir("hash-a"), dir("hash-b"));
        for d in [&a, &b] {
            std::fs::write(d.join("x.wav"), b"same bytes").unwrap();
        }
        let project = |d: &Path| {
            let mut p = Project::new("t", 48_000);
            let id = p.ids.allocate();
            p.sources.insert(
                id,
                AudioSource {
                    id,
                    name: "x".into(),
                    spec: SourceSpec::File {
                        path: d.join("x.wav"),
                        channels: 1,
                        frames: 1,
                        sample_rate: 48_000,
                    },
                },
            );
            p
        };
        let mut keys = Keys::inline();
        let (pa, mut pb) = (project(&a), project(&b));
        let ha = canonical_hash(&pa, &mut keys).unwrap();
        assert_eq!(ha, canonical_hash(&pb, &mut keys).unwrap());
        // Other ids handed out: the same project.
        let _: faderframe_core::TrackId = pb.ids.allocate();
        assert_eq!(ha, canonical_hash(&pb, &mut keys).unwrap());
        pb.name = "other".into();
        assert_ne!(ha, canonical_hash(&pb, &mut keys).unwrap());
    }
}
