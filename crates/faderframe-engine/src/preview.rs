//! Album playback: one stream file — the album as it will be delivered —
//! played to the outputs instead of the project.
//!
//! The control side hands the file over ([`crate::EngineController::set_preview`])
//! and drives play, pause and locate through [`PreviewShared`]'s atomics;
//! the audio thread reads the file wait-free at its own position (pages the
//! disk loader keeps resident round it; a page not there yet is silence)
//! and feeds the Tools meters as if it were the master. While a preview is
//! set the project's own output is muted, and its strip no longer feeds the
//! meters.

use faderframe_audio::DeviceBuffers;
use faderframe_audio_files::StreamSource;
use faderframe_realtime::ScopeRing;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

/// The file being played (owned by the audio thread, retired through the
/// garbage queue), with room to read a chunk of it.
pub struct Preview {
    pub source: Arc<StreamSource>,
    scratch: [Box<[f32]>; 2],
}

/// Frames read at a time.
const CHUNK: usize = 2048;

impl Preview {
    pub fn new(source: Arc<StreamSource>) -> Self {
        Self {
            source,
            scratch: [
                vec![0.0; CHUNK].into_boxed_slice(),
                vec![0.0; CHUNK].into_boxed_slice(),
            ],
        }
    }
}

/// No locate pending.
const NO_SEEK: i64 = i64::MIN;

/// Album playback's state, shared with the control side and the disk
/// loader.
#[derive(Debug)]
pub struct PreviewShared {
    active: AtomicBool,
    playing: AtomicBool,
    /// Frames into the file (written by the audio thread while playing).
    position: AtomicI64,
    seek: AtomicI64,
    frames: AtomicI64,
}

impl Default for PreviewShared {
    fn default() -> Self {
        Self {
            active: AtomicBool::new(false),
            playing: AtomicBool::new(false),
            position: AtomicI64::new(0),
            seek: AtomicI64::new(NO_SEEK),
            frames: AtomicI64::new(0),
        }
    }
}

impl PreviewShared {
    /// A file is set (the project is muted).
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }

    /// Where playback is (frames into the file).
    pub fn position(&self) -> i64 {
        self.position.load(Ordering::Relaxed)
    }

    /// The file's length (frames).
    pub fn frames(&self) -> i64 {
        self.frames.load(Ordering::Relaxed)
    }

    pub fn play(&self, on: bool) {
        if on && self.position() >= self.frames() {
            self.locate(0);
        }
        self.playing
            .store(on && self.is_active(), Ordering::Relaxed);
    }

    pub fn locate(&self, frame: i64) {
        let f = frame.clamp(0, self.frames().max(0));
        self.position.store(f, Ordering::Relaxed);
        self.seek.store(f, Ordering::Relaxed);
    }

    pub(crate) fn set(&self, frames: Option<i64>) {
        self.playing.store(false, Ordering::Relaxed);
        self.frames.store(frames.unwrap_or(0), Ordering::Relaxed);
        self.position.store(0, Ordering::Relaxed);
        self.seek.store(0, Ordering::Relaxed);
        self.active.store(frames.is_some(), Ordering::Relaxed);
    }
}

impl Preview {
    /// Audio thread: put the next frames on the outputs (silence when
    /// paused), the first two channels; feed the scope as `scope_id` when it
    /// listens to that.
    pub(crate) fn render(
        &mut self,
        shared: &PreviewShared,
        io: &mut dyn DeviceBuffers,
        scope: &ScopeRing,
        scope_id: Option<u64>,
    ) {
        let frames = io.frames();
        for c in 0..io.output_channels() {
            io.output(c).fill(0.0);
        }
        let seek = shared.seek.swap(NO_SEEK, Ordering::Relaxed);
        let mut pos = if seek != NO_SEEK {
            seek
        } else {
            shared.position()
        };
        if !shared.is_playing() {
            shared.position.store(pos, Ordering::Relaxed);
            return;
        }
        let outs = io.output_channels();
        let channels = self.source.channels().max(1);
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(CHUNK);
            for c in 0..2 {
                let buf = &mut self.scratch[c][..n];
                buf.fill(0.0);
                self.source
                    .read_segments(c.min(channels - 1), pos, n, |off, len, seg| {
                        if let Some(seg) = seg {
                            buf[off..off + len].copy_from_slice(seg);
                        }
                    });
            }
            let [l, r] = &self.scratch;
            match outs {
                0 => {}
                1 => {
                    for (o, (a, b)) in io.output(0)[done..done + n]
                        .iter_mut()
                        .zip(l.iter().zip(r.iter()))
                    {
                        *o = 0.5 * (a + b);
                    }
                }
                _ => {
                    io.output(0)[done..done + n].copy_from_slice(&l[..n]);
                    io.output(1)[done..done + n].copy_from_slice(&r[..n]);
                }
            }
            if let Some(id) = scope_id {
                scope.push(id, &l[..n], &r[..n]);
            }
            pos += n as i64;
            done += n;
        }
        if pos >= shared.frames() {
            pos = shared.frames();
            shared.playing.store(false, Ordering::Relaxed);
        }
        shared.position.store(pos, Ordering::Relaxed);
    }
}
