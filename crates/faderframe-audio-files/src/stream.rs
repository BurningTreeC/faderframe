//! Disk-streamed audio sources.
//!
//! A [`StreamSource`] is a WAV media file plus a lock-free page table of
//! decoded pages ([`PAGE_FRAMES`] frames each). A loader thread keeps the
//! pages around the playhead resident; the audio thread reads resident
//! pages without blocking and treats missing pages as silence (counted as a
//! miss). Memory use is bounded by what the loader keeps resident, not by
//! file length.

use crate::wavstream::WavFile;
use faderframe_realtime::{Epoch, PageTable, Reclaimer};
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Frames per page (≈0.34 s at 48 kHz; 64 KiB per channel).
pub const PAGE_FRAMES: usize = 16_384;

/// One resident page: `PAGE_FRAMES` samples per channel (zero-padded at
/// the end of the file).
pub struct Page {
    channels: Vec<Box<[f32]>>,
}

pub struct StreamSource {
    file: WavFile,
    pages: PageTable<Page>,
    resident: AtomicUsize,
    misses: AtomicU64,
}

impl std::fmt::Debug for StreamSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamSource")
            .field("path", &self.file.path())
            .field("frames", &self.file.frames())
            .field("resident_pages", &self.resident_pages())
            .finish()
    }
}

impl StreamSource {
    pub fn open(path: &Path) -> std::io::Result<Arc<Self>> {
        let file = WavFile::open(path)?;
        let n_pages = (file.frames() as usize).div_ceil(PAGE_FRAMES);
        Ok(Arc::new(Self {
            pages: PageTable::new(n_pages),
            file,
            resident: AtomicUsize::new(0),
            misses: AtomicU64::new(0),
        }))
    }

    pub fn path(&self) -> &Path {
        self.file.path()
    }

    pub fn frames(&self) -> u64 {
        self.file.frames()
    }

    pub fn channels(&self) -> usize {
        self.file.channels()
    }

    pub fn sample_rate(&self) -> u32 {
        self.file.sample_rate()
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn resident_pages(&self) -> usize {
        self.resident.load(Ordering::Relaxed)
    }

    /// Reads that hit a non-resident page (audible as dropouts).
    pub fn misses(&self) -> u64 {
        self.misses.load(Ordering::Relaxed)
    }

    /// Pages covering frames `[start, end)`.
    pub fn page_range(&self, start: i64, end: i64) -> Range<usize> {
        let frames = self.frames() as i64;
        let a = start.clamp(0, frames) as usize / PAGE_FRAMES;
        let b = (end.clamp(0, frames) as usize).div_ceil(PAGE_FRAMES);
        a..b.max(a)
    }

    /// Audio thread: visit frames `[start, start + len)` of `channel` as
    /// contiguous segments. `f(offset, segment)` receives the offset into
    /// the request and the samples, or `None` for frames that are outside
    /// the file or not resident. Wait-free.
    #[inline]
    pub fn read_segments(
        &self,
        channel: usize,
        start: i64,
        len: usize,
        mut f: impl FnMut(usize, usize, Option<&[f32]>),
    ) {
        let end = start + len as i64;
        let frames = self.frames() as i64;
        let mut pos = start;
        while pos < end {
            if pos < 0 || pos >= frames {
                // Outside the file: one segment up to the file start/end.
                let stop = if pos < 0 { end.min(0) } else { end };
                f((pos - start) as usize, (stop - pos) as usize, None);
                pos = stop;
                continue;
            }
            let page = pos as usize / PAGE_FRAMES;
            let in_page = pos as usize % PAGE_FRAMES;
            let n = ((PAGE_FRAMES - in_page) as i64)
                .min(end - pos)
                .min(frames - pos) as usize;
            let offset = (pos - start) as usize;
            self.pages.with(page, |p| match p {
                Some(p) => {
                    let ch = &p.channels[channel.min(p.channels.len() - 1)];
                    f(offset, n, Some(&ch[in_page..in_page + n]));
                }
                None => {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                    f(offset, n, None);
                }
            });
            pos += n as i64;
        }
    }

    /// Audio thread: one sample (for interpolating readers). `None` when
    /// outside the file or not resident.
    #[inline]
    pub fn sample(&self, channel: usize, frame: i64) -> Option<f32> {
        if frame < 0 || frame >= self.frames() as i64 {
            return None;
        }
        let page = frame as usize / PAGE_FRAMES;
        self.pages.with(page, |p| {
            p.map(|p| p.channels[channel.min(p.channels.len() - 1)][frame as usize % PAGE_FRAMES])
        })
    }

    /// Loader: make pages resident. Returns how many were loaded.
    pub fn ensure(
        &self,
        pages: Range<usize>,
        epoch: &Epoch,
        reclaimer: &mut Reclaimer<Page>,
        scratch: &mut Vec<u8>,
    ) -> std::io::Result<usize> {
        let mut loaded = 0;
        for i in pages {
            if i >= self.pages.len() || self.pages.is_resident(i) {
                continue;
            }
            let mut channels: Vec<Box<[f32]>> = (0..self.channels())
                .map(|_| vec![0.0; PAGE_FRAMES].into_boxed_slice())
                .collect();
            {
                let mut slices: Vec<&mut [f32]> = channels.iter_mut().map(|c| &mut c[..]).collect();
                self.file
                    .read((i * PAGE_FRAMES) as u64, &mut slices, scratch)?;
            }
            if let Some(old) = self.pages.install(i, Box::new(Page { channels }), epoch) {
                reclaimer.push(old);
            } else {
                self.resident.fetch_add(1, Ordering::Relaxed);
            }
            loaded += 1;
        }
        Ok(loaded)
    }

    /// Loader: evict every resident page not inside one of `keep`.
    pub fn evict_except(
        &self,
        keep: &[Range<usize>],
        epoch: &Epoch,
        reclaimer: &mut Reclaimer<Page>,
    ) -> usize {
        let mut evicted = 0;
        for i in 0..self.pages.len() {
            if keep.iter().any(|r| r.contains(&i)) {
                continue;
            }
            if let Some(old) = self.pages.evict(i, epoch) {
                reclaimer.push(old);
                self.resident.fetch_sub(1, Ordering::Relaxed);
                evicted += 1;
            }
        }
        evicted
    }

    /// Resident memory in bytes.
    pub fn resident_bytes(&self) -> usize {
        self.resident_pages() * PAGE_FRAMES * self.channels() * std::mem::size_of::<f32>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wav::WavFormat;

    #[test]
    fn segments_cover_requests_and_missing_pages_read_as_none() {
        let path = std::env::temp_dir().join(format!("ff-stream-{}.wav", std::process::id()));
        let n = PAGE_FRAMES * 3 + 100;
        let data: Vec<f32> = (0..n).map(|i| i as f32).collect();
        crate::write_wav(
            &path,
            std::slice::from_ref(&data),
            48_000,
            WavFormat::Float32,
            false,
        )
        .unwrap();
        let s = StreamSource::open(&path).unwrap();
        assert_eq!(s.page_count(), 4);
        let epoch = Epoch::new();
        let mut rec = Reclaimer::default();
        let mut scratch = Vec::new();
        // Nothing resident: everything is a miss.
        let mut got = vec![-1.0f32; 64];
        s.read_segments(0, 100, 64, |off, len, seg| {
            assert!(seg.is_none());
            got[off..off + len].fill(0.0);
        });
        assert!(s.misses() > 0);
        assert_eq!(
            s.ensure(
                s.page_range(PAGE_FRAMES as i64 - 10, PAGE_FRAMES as i64 + 10),
                &epoch,
                &mut rec,
                &mut scratch
            )
            .unwrap(),
            2
        );
        // A request spanning a page boundary and the file end.
        let start = PAGE_FRAMES as i64 - 10;
        let mut out = [0.0f32; 20];
        s.read_segments(0, start, 20, |off, len, seg| {
            out[off..off + len].copy_from_slice(seg.unwrap());
        });
        assert_eq!(out[0], (PAGE_FRAMES - 10) as f32);
        assert_eq!(out[19], (PAGE_FRAMES + 9) as f32);
        assert_eq!(s.sample(0, PAGE_FRAMES as i64), Some(PAGE_FRAMES as f32));
        assert_eq!(s.sample(0, -1), None);
        let mut tail = Vec::new();
        s.read_segments(0, n as i64 - 5, 10, |_, len, seg| {
            tail.push((len, seg.is_some()))
        });
        assert_eq!(
            tail,
            vec![(5, false), (5, false)],
            "last page not resident, then past the end"
        );
        assert_eq!(s.evict_except(&[], &epoch, &mut rec), 2);
        assert_eq!(s.resident_pages(), 0);
        epoch.advance();
        rec.collect(&epoch);
        std::fs::remove_file(&path).unwrap();
    }
}
