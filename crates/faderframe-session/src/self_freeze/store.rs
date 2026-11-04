//! The cache folder: `<project>/Cache` (a never-saved project's scratch
//! folder's `Cache` until the first save moves it), one float WAV per
//! cached chain named by its key, and `index.json` listing them with what
//! playing them needs and when each was last used. Over the size limit
//! the least recently used go first — never one being played.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// The cache folder's name beside a project file (and inside a scratch
/// media folder).
pub const CACHE_FOLDER: &str = "Cache";
const INDEX: &str = "index.json";

/// One cache file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Entry {
    /// Its file in the folder.
    pub file: String,
    pub frames: u64,
    pub channels: u16,
    pub rate: u32,
    /// Timeline sample (at `rate`) of its first frame.
    pub start: i64,
    /// Latency at its front (see `faderframe_engine::cache::CachedChain`).
    pub file_latency: u32,
    /// The chain's latency where it was rendered.
    pub chain_latency: u32,
    /// What the chain cost to run (a share of one core in real time).
    pub cost: f32,
    pub bytes: u64,
    /// Seconds since the Unix epoch it was last played.
    pub used: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Index {
    /// By key (hexadecimal).
    entries: BTreeMap<String, Entry>,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub(crate) fn key_name(key: u64) -> String {
    format!("{key:016x}")
}

/// The cache folder of a session (see the module docs).
#[derive(Debug, Default)]
pub(crate) struct Store {
    dir: PathBuf,
    index: Index,
}

impl Store {
    /// The folder at `dir` (its index read; files it lists that are gone
    /// are forgotten).
    pub fn open(dir: &Path) -> Self {
        let mut index: Index = std::fs::read_to_string(dir.join(INDEX))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        index.entries.retain(|_, e| dir.join(&e.file).is_file());
        Self {
            dir: dir.to_path_buf(),
            index,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn get(&self, key: u64) -> Option<&Entry> {
        self.index.entries.get(&key_name(key))
    }

    pub fn path_of(&self, e: &Entry) -> PathBuf {
        self.dir.join(&e.file)
    }

    /// Where a render for `key` writes before it is listed (a name of its
    /// own: renders never overwrite a listed file).
    pub fn temp_path(&self, key: u64) -> PathBuf {
        self.dir
            .join(format!(".{}-{}.part", key_name(key), std::process::id()))
    }

    /// The file a render for `key` ends up as.
    pub fn file_for(&self, key: u64) -> String {
        format!("{}.wav", key_name(key))
    }

    /// List a finished render.
    pub fn add(&mut self, key: u64, mut e: Entry) {
        e.used = now();
        self.index.entries.insert(key_name(key), e);
        self.save();
    }

    /// Played now (for the least-recently-used order).
    pub fn touch(&mut self, key: u64) {
        if let Some(e) = self.index.entries.get_mut(&key_name(key)) {
            e.used = now();
            self.save();
        }
    }

    pub fn bytes(&self) -> u64 {
        self.index.entries.values().map(|e| e.bytes).sum()
    }

    pub fn len(&self) -> usize {
        self.index.entries.len()
    }

    /// Remove the least recently used files until at most `limit` bytes
    /// are kept, never those in `keep`. Returns how many went.
    pub fn evict(&mut self, limit: u64, keep: &HashSet<u64>) -> usize {
        let keep: HashSet<String> = keep.iter().map(|k| key_name(*k)).collect();
        let mut order: Vec<(u64, String)> = self
            .index
            .entries
            .iter()
            .filter(|(k, _)| !keep.contains(*k))
            .map(|(k, e)| (e.used, k.clone()))
            .collect();
        order.sort();
        let mut total = self.bytes();
        let mut gone = 0;
        for (_, k) in order {
            if total <= limit {
                break;
            }
            if let Some(e) = self.index.entries.remove(&k) {
                let _ = std::fs::remove_file(self.dir.join(&e.file));
                total = total.saturating_sub(e.bytes);
                gone += 1;
            }
        }
        if gone > 0 {
            self.save();
        }
        gone
    }

    /// The files of every entry but those in `keep`, with their sizes.
    pub fn files_except(&self, keep: &HashSet<u64>) -> Vec<(PathBuf, u64)> {
        let keep: HashSet<String> = keep.iter().map(|k| key_name(*k)).collect();
        self.index
            .entries
            .iter()
            .filter(|(k, _)| !keep.contains(*k))
            .map(|(_, e)| (self.dir.join(&e.file), e.bytes))
            .collect()
    }

    /// Remove every file but those in `keep` (cleaning up with unused
    /// media). Returns the bytes freed.
    pub fn keep_only(&mut self, keep: &HashSet<u64>) -> u64 {
        let keep: HashSet<String> = keep.iter().map(|k| key_name(*k)).collect();
        let gone: Vec<String> = self
            .index
            .entries
            .keys()
            .filter(|k| !keep.contains(*k))
            .cloned()
            .collect();
        let mut freed = 0;
        for k in gone {
            if let Some(e) = self.index.entries.remove(&k) {
                let _ = std::fs::remove_file(self.dir.join(&e.file));
                freed += e.bytes;
            }
        }
        // Renders that never finished (a crash) are not listed.
        if let Ok(files) = std::fs::read_dir(&self.dir) {
            for f in files.flatten() {
                let name = f.file_name().to_string_lossy().to_string();
                if name.ends_with(".part") {
                    let _ = std::fs::remove_file(f.path());
                }
            }
        }
        self.save();
        freed
    }

    /// Move the folder to `to` (a save put the project there): its files
    /// renamed (or copied across file systems; open streams keep their
    /// handles), merged with what `to` lists already.
    pub fn move_to(&mut self, to: &Path) {
        if to == self.dir {
            return;
        }
        let mut moved = Store::open(to);
        if std::fs::create_dir_all(to).is_err() {
            *self = moved;
            return;
        }
        for (k, e) in std::mem::take(&mut self.index.entries) {
            let from = self.dir.join(&e.file);
            let dest = to.join(&e.file);
            let ok = std::fs::rename(&from, &dest).is_ok()
                || (std::fs::copy(&from, &dest).is_ok() && {
                    let _ = std::fs::remove_file(&from);
                    true
                });
            if ok {
                moved.index.entries.insert(k, e);
            }
        }
        let _ = std::fs::remove_file(self.dir.join(INDEX));
        let _ = std::fs::remove_dir(&self.dir);
        moved.save();
        *self = moved;
    }

    fn save(&self) {
        if self.index.entries.is_empty() && !self.dir.exists() {
            return;
        }
        if std::fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        if let Ok(text) = serde_json::to_string_pretty(&self.index)
            && let Err(e) = std::fs::write(self.dir.join(INDEX), text)
        {
            tracing::warn!("self-freeze: {}: {e}", self.dir.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(file: &str, bytes: u64, used: u64) -> Entry {
        Entry {
            file: file.into(),
            frames: 1,
            channels: 2,
            rate: 48_000,
            start: 0,
            file_latency: 0,
            chain_latency: 0,
            cost: 0.1,
            bytes,
            used,
        }
    }

    #[test]
    fn the_least_recently_used_go_first_and_never_those_kept() {
        let dir = std::env::temp_dir().join(format!("ff-self-freeze-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut s = Store::open(&dir);
        for (k, used) in [(1u64, 30u64), (2, 10), (3, 20)] {
            std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
            let file = s.file_for(k);
            std::fs::write(dir.join(&file), [0u8; 4]).unwrap_or_else(|e| panic!("{e}"));
            s.index.entries.insert(key_name(k), entry(&file, 100, used));
        }
        // Over 150 bytes: 2 is the oldest but kept, so 3 and then 1 go.
        assert_eq!(s.evict(150, &HashSet::from([2])), 2);
        assert!(s.get(2).is_some() && s.get(1).is_none() && s.get(3).is_none());
        assert!(!dir.join(s.file_for(3)).exists());
        // Listed again from the folder.
        s.save();
        let again = Store::open(&dir);
        assert_eq!(again.len(), 1);
        // Moving keeps it.
        let to = dir.with_extension("moved");
        let mut s = again;
        s.move_to(&to);
        assert!(s.get(2).is_some_and(|e| to.join(&e.file).is_file()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&to);
    }
}
