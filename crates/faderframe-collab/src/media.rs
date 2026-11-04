//! Media files fetched from the host by their content.
//!
//! A peer asks for a file by its content key (the session's content hash)
//! from the byte a part kept from before ends at. The host sends `Begin`
//! (the length), chunks of [`CHUNK`] bytes in between everything else on
//! the connection (edits and presence first: a long file never holds up
//! an edit), and `End` with the SHA-256 of the whole file. The peer writes
//! into `<file>.part`, hashes what it has, and only a file whose hash is
//! the host's takes its name; a part left by a lost connection is where
//! the next request starts.

use crate::wire::MediaPart;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

/// Bytes a chunk carries.
pub const CHUNK: usize = 256 << 10;

/// The files the host lets peers fetch, by content key.
#[derive(Clone, Default)]
pub struct Shelf(Arc<Mutex<HashMap<String, PathBuf>>>);

impl Shelf {
    pub fn put(&self, key: &str, path: &Path) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.to_string(), path.to_path_buf());
    }

    pub fn get(&self, key: &str) -> Option<PathBuf> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .cloned()
    }

    pub fn len(&self) -> usize {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A file on its way out (host side), a chunk of work at a time.
pub(crate) struct Sending {
    pub key: String,
    file: File,
    len: u64,
    /// Where the peer's part ends: bytes before it are hashed, not sent.
    from: u64,
    /// Bytes read so far (hashed).
    pos: u64,
    hash: Sha256,
    begun: bool,
    done: bool,
    buf: Vec<u8>,
}

impl Sending {
    pub fn open(key: &str, path: &Path, from: u64) -> io::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Self {
            key: key.to_string(),
            file,
            len,
            from: from.min(len),
            pos: 0,
            hash: Sha256::new(),
            begun: false,
            done: false,
            buf: vec![0; CHUNK],
        })
    }

    /// The next piece of work: a message to send, or a chunk hashed
    /// (before the peer's part ends: nothing to send), or done. Each call
    /// reads at most one chunk.
    pub fn next_part(&mut self) -> io::Result<Work> {
        if !self.begun {
            self.begun = true;
            return Ok(Work::Send(MediaPart::Begin {
                key: self.key.clone(),
                len: self.len,
                from: self.from,
            }));
        }
        if self.done {
            return Ok(Work::Done);
        }
        if self.pos >= self.len {
            self.done = true;
            let sha256 = std::mem::take(&mut self.hash).finalize().into();
            return Ok(Work::Send(MediaPart::End {
                key: self.key.clone(),
                sha256,
            }));
        }
        let limit = if self.pos < self.from {
            self.from
        } else {
            self.len
        };
        let n = ((limit - self.pos) as usize).min(CHUNK);
        self.file.seek(SeekFrom::Start(self.pos))?;
        self.file.read_exact(&mut self.buf[..n])?;
        self.hash.update(&self.buf[..n]);
        let offset = self.pos;
        self.pos += n as u64;
        if offset < self.from {
            return Ok(Work::Hashed);
        }
        Ok(Work::Send(MediaPart::Chunk {
            key: self.key.clone(),
            offset,
            data: self.buf[..n].to_vec(),
        }))
    }
}

/// See [`Sending::next_part`].
#[derive(Debug)]
pub(crate) enum Work {
    Send(MediaPart),
    Hashed,
    Done,
}

/// What became of a fetch.
#[derive(Clone, Debug, PartialEq)]
pub enum Fetched {
    /// `done` of `len` bytes are here.
    Progress {
        key: String,
        done: u64,
        len: u64,
    },
    Arrived {
        key: String,
        path: PathBuf,
    },
    Failed {
        key: String,
        reason: String,
    },
}

struct Receiving {
    dest: PathBuf,
    file: Option<File>,
    hash: Sha256,
    len: u64,
    pos: u64,
    /// Bytes at the last progress report.
    told: u64,
}

/// The fetches of a peer (keyed by content), shared by the session (which
/// asks) and the connection's reader (which writes what arrives).
#[derive(Clone, Default)]
pub struct Fetches(Arc<Mutex<HashMap<String, Receiving>>>);

/// A part kept from an earlier, unfinished fetch.
pub fn part_of(dest: &Path) -> PathBuf {
    let mut p = dest.as_os_str().to_owned();
    p.push(".part");
    PathBuf::from(p)
}

impl Fetches {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Receiving>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Note a fetch of `key` into `dest`: the byte to ask from (where a
    /// part kept from before ends).
    pub fn start(&self, key: &str, dest: &Path) -> u64 {
        let from = std::fs::metadata(part_of(dest)).map_or(0, |m| m.len());
        self.lock().insert(
            key.to_string(),
            Receiving {
                dest: dest.to_path_buf(),
                file: None,
                hash: Sha256::new(),
                len: 0,
                pos: 0,
                told: 0,
            },
        );
        from
    }

    pub fn is_fetching(&self, key: &str) -> bool {
        self.lock().contains_key(key)
    }

    /// How many fetches are under way.
    pub fn count(&self) -> usize {
        self.lock().len()
    }

    /// Forget every fetch (the connection went; parts stay on disk).
    pub fn clear(&self) {
        self.lock().clear();
    }

    /// Take in a part from the host.
    pub fn receive(&self, part: MediaPart) -> Option<Fetched> {
        let key = match &part {
            MediaPart::Begin { key, .. }
            | MediaPart::Chunk { key, .. }
            | MediaPart::End { key, .. }
            | MediaPart::Missing { key } => key.clone(),
        };
        let mut all = self.lock();
        let r = all.get_mut(&key)?;
        let result = (|| -> io::Result<Option<Fetched>> {
            match part {
                MediaPart::Begin { len, from, .. } => {
                    let part = part_of(&r.dest);
                    if let Some(dir) = part.parent() {
                        std::fs::create_dir_all(dir)?;
                    }
                    let mut f = std::fs::OpenOptions::new()
                        .create(true)
                        .truncate(false)
                        .read(true)
                        .write(true)
                        .open(&part)?;
                    // What is kept is hashed again; anything past `from`
                    // (none, normally) goes.
                    f.set_len(from)?;
                    f.seek(SeekFrom::Start(0))?;
                    let mut buf = vec![0; CHUNK];
                    let mut left = from;
                    while left > 0 {
                        let n = (left as usize).min(CHUNK);
                        f.read_exact(&mut buf[..n])?;
                        r.hash.update(&buf[..n]);
                        left -= n as u64;
                    }
                    r.file = Some(f);
                    r.len = len;
                    r.pos = from;
                    r.told = from;
                    Ok(Some(Fetched::Progress {
                        key: key.clone(),
                        done: from,
                        len,
                    }))
                }
                MediaPart::Chunk { offset, data, .. } => {
                    let Some(f) = r.file.as_mut() else {
                        return Err(io::Error::other("a chunk before the file began"));
                    };
                    if offset != r.pos {
                        return Err(io::Error::other(format!(
                            "a chunk at {offset}, expected {}",
                            r.pos
                        )));
                    }
                    f.write_all(&data)?;
                    r.hash.update(&data);
                    r.pos += data.len() as u64;
                    if r.pos - r.told >= 4 << 20 {
                        r.told = r.pos;
                        return Ok(Some(Fetched::Progress {
                            key: key.clone(),
                            done: r.pos,
                            len: r.len,
                        }));
                    }
                    Ok(None)
                }
                MediaPart::End { sha256, .. } => {
                    let part = part_of(&r.dest);
                    if let Some(mut f) = r.file.take() {
                        f.flush()?;
                        f.sync_all()?;
                    }
                    let got: [u8; 32] = std::mem::take(&mut r.hash).finalize().into();
                    if got != sha256 || r.pos != r.len {
                        let _ = std::fs::remove_file(&part);
                        return Err(io::Error::other(
                            "the file arrived damaged (its SHA-256 differs from the host's)",
                        ));
                    }
                    std::fs::rename(&part, &r.dest)?;
                    Ok(Some(Fetched::Arrived {
                        key: key.clone(),
                        path: r.dest.clone(),
                    }))
                }
                MediaPart::Missing { .. } => Err(io::Error::other("the host does not have it")),
            }
        })();
        match result {
            Ok(Some(f @ (Fetched::Arrived { .. } | Fetched::Failed { .. }))) => {
                all.remove(&key);
                Some(f)
            }
            Ok(other) => other,
            Err(e) => {
                all.remove(&key);
                Some(Fetched::Failed {
                    key,
                    reason: e.to_string(),
                })
            }
        }
    }
}

/// The SHA-256 of bytes.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// The SHA-256 of a whole file.
pub fn sha256_of(path: &Path) -> io::Result<[u8; 32]> {
    let mut f = File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0; CHUNK];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            return Ok(h.finalize().into());
        }
        h.update(&buf[..n]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ff-collab-media-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn file(d: &Path, len: usize) -> PathBuf {
        let p = d.join("source.wav");
        let bytes: Vec<u8> = (0..len).map(|i| (i * 31 % 251) as u8).collect();
        std::fs::write(&p, bytes).unwrap();
        p
    }

    fn send_all(s: &mut Sending, f: &Fetches, stop_after: Option<usize>) -> Option<Fetched> {
        let mut last = None;
        let mut chunks = 0;
        loop {
            let p = match s.next_part().unwrap() {
                Work::Send(p) => p,
                Work::Hashed => continue,
                Work::Done => return last,
            };
            if matches!(&p, MediaPart::Chunk { .. }) {
                chunks += 1;
                if stop_after.is_some_and(|n| chunks > n) {
                    return last;
                }
            }
            if let Some(r) = f.receive(p) {
                last = Some(r);
            }
        }
    }

    #[test]
    fn a_file_arrives_whole_and_a_broken_fetch_resumes() {
        let d = dir("resume");
        let src = file(&d, CHUNK * 5 + 123);
        let dest = d.join("here").join("copy.wav");
        let fetches = Fetches::default();
        // The connection goes after two chunks: the part stays.
        let from = fetches.start("k", &dest);
        assert_eq!(from, 0);
        let mut s = Sending::open("k", &src, from).unwrap();
        send_all(&mut s, &fetches, Some(2));
        fetches.clear();
        assert_eq!(
            std::fs::metadata(part_of(&dest)).unwrap().len(),
            CHUNK as u64 * 2
        );
        // Asked again: from where the part ends.
        let from = fetches.start("k", &dest);
        assert_eq!(from, CHUNK as u64 * 2);
        let mut s = Sending::open("k", &src, from).unwrap();
        let r = send_all(&mut s, &fetches, None);
        assert_eq!(
            r,
            Some(Fetched::Arrived {
                key: "k".into(),
                path: dest.clone()
            })
        );
        assert_eq!(std::fs::read(&dest).unwrap(), std::fs::read(&src).unwrap());
        assert!(!part_of(&dest).exists());
        assert_eq!(sha256_of(&dest).unwrap(), sha256_of(&src).unwrap());
    }

    #[test]
    fn a_damaged_part_is_thrown_away() {
        let d = dir("damaged");
        let src = file(&d, CHUNK + 10);
        let dest = d.join("copy.wav");
        // A part that is not the start of this file.
        std::fs::write(part_of(&dest), vec![0xee; 100]).unwrap();
        let fetches = Fetches::default();
        let from = fetches.start("k", &dest);
        let mut s = Sending::open("k", &src, from).unwrap();
        let r = send_all(&mut s, &fetches, None);
        assert!(matches!(r, Some(Fetched::Failed { .. })), "{r:?}");
        assert!(!part_of(&dest).exists() && !dest.exists());
        // The next try starts over and succeeds.
        let from = fetches.start("k", &dest);
        assert_eq!(from, 0);
        let mut s = Sending::open("k", &src, from).unwrap();
        assert!(matches!(
            send_all(&mut s, &fetches, None),
            Some(Fetched::Arrived { .. })
        ));
    }
}
