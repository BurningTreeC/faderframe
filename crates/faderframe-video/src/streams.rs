//! Pipelines: a file parsed into its elementary streams (`parsebin`), the
//! streams a job wants linked (decoded or not), the rest discarded, and the
//! bus watched for the end, errors and cancellation.

use crate::{Result, VideoError};
use gst::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How long a job may go without moving before it counts as stalled.
pub(crate) const STALL: Duration = Duration::from_secs(30);

/// What a parsed stream carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Picture,
    Sound,
    Other,
}

pub(crate) fn kind_of(caps: &gst::CapsRef) -> Kind {
    let Some(s) = caps.structure(0) else {
        return Kind::Other;
    };
    let name = s.name();
    if name.starts_with("video/") || name.starts_with("image/") {
        Kind::Picture
    } else if name.starts_with("audio/") {
        Kind::Sound
    } else {
        Kind::Other
    }
}

/// A pipeline set to `NULL` when dropped.
pub(crate) struct Pipeline {
    pub(crate) pipeline: gst::Pipeline,
    pub(crate) path: PathBuf,
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

pub(crate) fn make(factory: &str) -> Result<gst::Element> {
    gst::ElementFactory::make(factory)
        .build()
        .map_err(|_| VideoError::Missing(factory.to_string()))
}

impl Pipeline {
    pub(crate) fn new(path: &Path) -> Result<Self> {
        crate::init()?;
        Ok(Self {
            pipeline: gst::Pipeline::new(),
            path: path.to_path_buf(),
        })
    }

    /// `filesrc ! parsebin`; `route` is asked, for each stream parsed (in
    /// the file's order, counted per kind), for the pad to link it to
    /// (`None`: discarded). It runs on a streaming thread.
    pub(crate) fn parse(
        &self,
        route: impl Fn(&Pipeline, Kind, usize, &gst::Pad) -> Option<gst::Pad> + Send + Sync + 'static,
    ) -> Result<()> {
        if !self.path.is_file() {
            return Err(VideoError::media(&self.path, "no such file"));
        }
        let src = gst::ElementFactory::make("filesrc")
            .property("location", self.path.to_string_lossy().as_ref())
            .build()
            .map_err(|_| VideoError::Missing("filesrc".into()))?;
        let parse = make("parsebin")?;
        self.pipeline.add_many([&src, &parse])?;
        src.link(&parse)?;
        let weak = self.pipeline.downgrade();
        let path = self.path.clone();
        let counts = std::sync::Mutex::new([0usize; 3]);
        parse.connect_pad_added(move |_, pad| {
            let Some(pipeline) = weak.upgrade() else {
                return;
            };
            let caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
            let kind = kind_of(&caps);
            let n = {
                let Ok(mut c) = counts.lock() else { return };
                let slot = match kind {
                    Kind::Picture => 0,
                    Kind::Sound => 1,
                    Kind::Other => 2,
                };
                c[slot] += 1;
                c[slot] - 1
            };
            // A short-lived view for the router (the pipeline is not ours
            // to stop here).
            let view = std::mem::ManuallyDrop::new(Pipeline {
                pipeline: pipeline.clone(),
                path: path.clone(),
            });
            match route(&view, kind, n, pad) {
                Some(sink) => {
                    if let Err(e) = pad.link(&sink) {
                        tracing::warn!("{}: a stream did not link: {e:?}", path.display());
                    }
                }
                None => discard(&pipeline, pad),
            }
        });
        Ok(())
    }

    /// A decoder for a parsed stream, its raw output linked to `next` (the
    /// sink pad of what follows); the decoder's sink pad.
    pub(crate) fn decoder_into(&self, next: gst::Pad) -> Result<gst::Pad> {
        let decode = make("decodebin")?;
        self.pipeline.add(&decode)?;
        decode.connect_pad_added(move |_, pad| {
            if !next.is_linked()
                && let Err(e) = pad.link(&next)
            {
                tracing::warn!("a decoded stream did not link: {e:?}");
            }
        });
        decode.sync_state_with_parent()?;
        decode
            .static_pad("sink")
            .ok_or_else(|| VideoError::Gst("decodebin without a sink".into()))
    }

    /// Run until the end (or `cancel`), calling `tick` with the position's
    /// share of the length about every 100 ms; a pipeline whose position
    /// has not moved for [`STALL`] has stalled (an error).
    pub(crate) fn run(&self, cancel: &AtomicBool, mut tick: impl FnMut(f64)) -> Result<()> {
        let mut last = (None, std::time::Instant::now());
        self.pipeline.set_state(gst::State::Playing)?;
        let bus = self
            .pipeline
            .bus()
            .ok_or_else(|| VideoError::Gst("a pipeline without a bus".into()))?;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(VideoError::Cancelled);
            }
            if let Some(msg) = bus.timed_pop_filtered(
                gst::ClockTime::from_mseconds(100),
                &[gst::MessageType::Eos, gst::MessageType::Error],
            ) {
                match msg.view() {
                    gst::MessageView::Eos(_) => return Ok(()),
                    gst::MessageView::Error(e) => return Err(self.error(e)),
                    _ => {}
                }
            }
            let pos = self.pipeline.query_position::<gst::ClockTime>();
            if pos != last.0 {
                last = (pos, std::time::Instant::now());
            } else if last.1.elapsed() > STALL {
                return Err(VideoError::media(&self.path, "stalled"));
            }
            let len = self.pipeline.query_duration::<gst::ClockTime>();
            if let (Some(p), Some(l)) = (pos, len)
                && l.nseconds() > 0
            {
                tick((p.nseconds() as f64 / l.nseconds() as f64).clamp(0.0, 1.0));
            }
        }
    }

    /// Wait for the pipeline to settle after a state change or seek.
    pub(crate) fn settle(&self, timeout: Duration) -> Result<()> {
        let bus = self
            .pipeline
            .bus()
            .ok_or_else(|| VideoError::Gst("a pipeline without a bus".into()))?;
        let msg = bus
            .timed_pop_filtered(
                gst::ClockTime::from_nseconds(timeout.as_nanos() as u64),
                &[
                    gst::MessageType::AsyncDone,
                    gst::MessageType::Error,
                    gst::MessageType::Eos,
                ],
            )
            .ok_or_else(|| VideoError::media(&self.path, "timed out"))?;
        match msg.view() {
            gst::MessageView::Error(e) => Err(self.error(e)),
            // Prerolled (or nothing left to show).
            _ => Ok(()),
        }
    }

    pub(crate) fn error(&self, e: &gst::message::Error) -> VideoError {
        let detail = e.debug().map(|d| format!(" ({d})")).unwrap_or_default();
        tracing::debug!("{}: {}{detail}", self.path.display(), e.error());
        VideoError::media(&self.path, e.error().to_string())
    }
}

/// A stream nobody wants ends in a fakesink (an unlinked stream would stop
/// the others) behind a leaky one-buffer queue: a paused sink holds its
/// first buffer, which would stall a demuxer's one streaming thread before
/// the wanted stream's first frame arrives.
fn discard(pipeline: &gst::Pipeline, pad: &gst::Pad) {
    let (Ok(queue), Ok(sink)) = (
        gst::ElementFactory::make("queue")
            .property_from_str("leaky", "downstream")
            .property("max-size-buffers", 1u32)
            .property("max-size-bytes", 0u32)
            .property("max-size-time", 0u64)
            .build(),
        gst::ElementFactory::make("fakesink")
            .property("sync", false)
            .property("async", false)
            .build(),
    ) else {
        return;
    };
    if pipeline.add_many([&queue, &sink]).is_err() || queue.link(&sink).is_err() {
        return;
    }
    let _ = sink.sync_state_with_parent();
    let _ = queue.sync_state_with_parent();
    if let Some(sinkpad) = queue.static_pad("sink") {
        let _ = pad.link(&sinkpad);
    }
}
