//! The picture on an output of its own (a DeckLink card's SDI or HDMI —
//! `faderframe_video::output`): a thread pushes a frame per output frame,
//! the sink pacing it, each one the picture for when the output will show
//! it — the engine's position then, less the sound's latency, as the
//! video window does. The session publishes what plays where (the top
//! shown video track's clips) for the thread on every tick.

use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_video::Want;
use faderframe_video::output::{Output, Sink};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// A clip of the picture: the timeline span it covers (samples), its
/// video's key and its file time at the start (ns).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Span {
    start: i64,
    end: i64,
    key: u64,
    offset: i64,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct PictureMap {
    rate: u32,
    /// The picture's offset against the sound (ns, positive: later).
    offset_ns: i64,
    spans: Vec<Span>,
}

impl PictureMap {
    /// The video and file time at timeline position `pos`.
    fn at(&self, pos: i64) -> Option<(u64, i64)> {
        let s = self.spans.iter().find(|s| pos >= s.start && pos < s.end)?;
        let ns = (pos - s.start) as i128 * 1_000_000_000 / self.rate.max(1) as i128;
        Some((s.key, s.offset + ns as i64))
    }
}

/// A picture output running.
pub(crate) struct PictureOut {
    sink: Sink,
    map: Arc<Mutex<PictureMap>>,
    stop: Arc<AtomicBool>,
    frames: Arc<AtomicU64>,
    /// The last picture's frame number + 1 (0: black).
    last: Arc<AtomicU64>,
    error: Arc<Mutex<Option<String>>>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for PictureOut {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Session {
    /// Show the picture on `sink` too (`None`: stop).
    pub fn set_picture_output(&mut self, sink: Option<Sink>) -> Result<()> {
        self.picture_out = None;
        let Some(sink) = sink else {
            self.revision += 1;
            return Ok(());
        };
        let mut output = Output::open(&sink).map_err(|e| SessionError::Other(e.to_string()))?;
        let frames = self.video_service().handle();
        let clock = self.engine.clock();
        let midi = self.midi.sender.clock();
        let map = Arc::new(Mutex::new(PictureMap::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let pushed = Arc::new(AtomicU64::new(0));
        let last = Arc::new(AtomicU64::new(0));
        let error = Arc::new(Mutex::new(None));
        let (m, st, n, l, err) = (
            Arc::clone(&map),
            Arc::clone(&stop),
            Arc::clone(&pushed),
            Arc::clone(&last),
            Arc::clone(&error),
        );
        let thread = std::thread::Builder::new()
            .name("faderframe-picture-out".into())
            .spawn(move || {
                let size = output.size();
                while !st.load(Ordering::Relaxed) {
                    let map = m.lock().map(|g| g.clone()).unwrap_or_default();
                    let lead = output.lead_ns();
                    let now = midi.now_ns() as i64;
                    let at = (now + lead - map.offset_ns).max(0) as u64;
                    let playing = clock.playing();
                    let picture = clock.position_at(at).and_then(|p| {
                        let latency = clock.output_latency() as f64 * clock.speed();
                        let pos = p - latency.round() as i64;
                        let (key, t) = map.at(pos)?;
                        let want = if playing { Want::Play } else { Want::Still };
                        frames.picture(key, t, size, want)
                    });
                    let frame = picture.as_ref().map(|p| &*p.frame);
                    l.store(
                        picture.as_ref().map_or(0, |p| p.number as u64 + 1),
                        Ordering::Relaxed,
                    );
                    if let Err(e) = output.push(frame) {
                        if let Ok(mut g) = err.lock() {
                            *g = Some(e.to_string());
                        }
                        break;
                    }
                    n.fetch_add(1, Ordering::Relaxed);
                }
            })
            .map_err(|e| SessionError::Other(e.to_string()))?;
        self.picture_out = Some(PictureOut {
            sink,
            map,
            stop,
            frames: pushed,
            last,
            error,
            thread: Some(thread),
        });
        self.publish_picture_map();
        self.revision += 1;
        Ok(())
    }

    /// The picture output, the frames it has shown and the last one's
    /// picture (its frame number; `None`: black).
    pub fn picture_output(&self) -> Option<(&Sink, u64, Option<usize>)> {
        self.picture_out.as_ref().map(|o| {
            let last = o.last.load(Ordering::Relaxed);
            (
                &o.sink,
                o.frames.load(Ordering::Relaxed),
                last.checked_sub(1).map(|n| n as usize),
            )
        })
    }

    /// Tell the output what plays where (from the tick); an output that
    /// failed stops, and says why.
    pub(crate) fn publish_picture_map(&mut self) {
        let Some(out) = &self.picture_out else {
            return;
        };
        let failed = out.error.lock().ok().and_then(|mut e| e.take());
        if let Some(e) = failed {
            self.picture_out = None;
            self.notify(NoticeLevel::Error, format!("picture output stopped: {e}"));
            return;
        }
        let p = &self.project;
        let rate = p.sample_rate;
        let spans = p
            .video
            .shown_tracks()
            .next()
            .map(|t| {
                t.clips
                    .iter()
                    .map(|c| Span {
                        start: c.start,
                        end: c.start + faderframe_project::video::ns_to_samples(c.length, rate),
                        key: c.source.raw(),
                        offset: c.offset,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let map = PictureMap {
            rate,
            offset_ns: (p.video.offset_ms * 1e6) as i64,
            spans,
        };
        if let Ok(mut m) = out.map.lock()
            && *m != map
        {
            *m = map;
        }
    }
}
