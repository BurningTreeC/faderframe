//! A movie's sound as a project's media: one audio stream decoded through
//! the same demuxer as the picture, filled from the file's time zero (so a
//! stream starting late, or with gaps, stays in step with the picture),
//! then resampled and written like any import (float WAV and peaks).

use crate::streams::{Kind, Pipeline, make};
use crate::{Result, VideoError};
use faderframe_audio_files::import::{ImportProgress, ImportedAudio, peaks_path_for, unique_path};
use faderframe_audio_files::resample::StreamResampler;
use faderframe_audio_files::wavstream::WavWriter;
use faderframe_audio_files::{PeakBuilder, WavFormat};
use gst::prelude::*;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Write audio stream `stream` (0: the first) of `src` into `dest_dir` at
/// `rate`.
pub fn extract_audio(
    src: &Path,
    stream: usize,
    dest_dir: &Path,
    rate: u32,
    progress: &ImportProgress,
    cancel: &AtomicBool,
) -> Result<ImportedAudio> {
    let p = Pipeline::new(src)?;
    let convert = make("audioconvert")?;
    // From the segment's start, gaps filled: the sound keeps the picture's
    // time.
    let fill = gst::ElementFactory::make("audiorate")
        .property("skip-to-first", false)
        .build()
        .map_err(|_| VideoError::Missing("audiorate".into()))?;
    let caps = gst::Caps::builder("audio/x-raw")
        .field("format", "F32LE")
        .field("layout", "interleaved")
        .build();
    let sink = gst_app::AppSink::builder()
        .caps(&caps)
        .sync(false)
        .max_buffers(16)
        .drop(false)
        .build();
    let elements = [convert, fill, sink.clone().upcast()];
    p.pipeline.add_many(&elements)?;
    gst::Element::link_many(&elements)?;
    let next = elements[0]
        .static_pad("sink")
        .ok_or_else(|| VideoError::Gst("audioconvert without a sink".into()))?;
    p.parse(move |p, kind, n, pad| {
        (kind == Kind::Sound && n == stream)
            .then(|| p.decoder_into(pad, next.clone()).ok())
            .flatten()
    })?;
    p.pipeline.set_state(gst::State::Playing)?;

    std::fs::create_dir_all(dest_dir)?;
    let name = src
        .file_stem()
        .map_or_else(|| "video".to_string(), |s| s.to_string_lossy().into_owned());
    let out = unique_path(dest_dir, &name, "wav");
    struct Out {
        writer: WavWriter,
        peaks: PeakBuilder,
        resampler: Option<StreamResampler>,
        original_rate: u32,
        channels: usize,
    }
    let mut state: Option<Out> = None;
    let bus = p
        .pipeline
        .bus()
        .ok_or_else(|| VideoError::Gst("a pipeline without a bus".into()))?;
    let fail = |e: VideoError| {
        let _ = std::fs::remove_file(&out);
        e
    };
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(fail(VideoError::Cancelled));
        }
        if let Some(msg) = bus.pop_filtered(&[gst::MessageType::Error])
            && let gst::MessageView::Error(e) = msg.view()
        {
            return Err(fail(p.error(e)));
        }
        let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_mseconds(200)) else {
            if sink.is_eos() {
                break;
            }
            continue;
        };
        let (Some(buffer), Some(caps)) = (sample.buffer(), sample.caps()) else {
            continue;
        };
        let Some(st) = caps.structure(0) else {
            continue;
        };
        let channels = st.get::<i32>("channels").unwrap_or(1).max(1) as usize;
        let sound_rate = st.get::<i32>("rate").unwrap_or(48_000).max(1) as u32;
        if state.is_none() {
            let original_rate = sound_rate;
            if let Some(len) = p.pipeline.query_duration::<gst::ClockTime>() {
                let frames = len.nseconds() as u128 * original_rate as u128 / 1_000_000_000;
                progress.total.store(frames as u64, Ordering::Relaxed);
            }
            state = Some(Out {
                writer: WavWriter::create(&out, channels as u16, rate, WavFormat::Float32, false)
                    .map_err(|e| fail(e.into()))?,
                peaks: PeakBuilder::new(channels),
                resampler: (original_rate != rate)
                    .then(|| StreamResampler::new(original_rate, rate, channels))
                    .transpose()
                    .map_err(|e| fail(VideoError::Import(e.into())))?,
                original_rate,
                channels,
            });
        }
        let Some(o) = state.as_mut() else { continue };
        if channels != o.channels {
            return Err(fail(VideoError::media(
                src,
                "the sound changed its channels",
            )));
        }
        let map = buffer.map_readable().map_err(|e| fail(e.into()))?;
        let samples: Vec<f32> = map
            .as_slice()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        let frames = samples.len() / channels.max(1);
        let planes: Vec<Vec<f32>> = (0..channels)
            .map(|c| samples.iter().skip(c).step_by(channels).copied().collect())
            .collect();
        progress.done.fetch_add(frames as u64, Ordering::Relaxed);
        let (writer, peaks) = (&mut o.writer, &mut o.peaks);
        let mut write = |s: &[&[f32]], n: usize| {
            writer.write_planar(s, n)?;
            peaks.push(s, n);
            Ok(())
        };
        match o.resampler.as_mut() {
            Some(r) => r.push(&planes, frames, &mut write),
            None => {
                let s: Vec<&[f32]> = planes.iter().map(|c| c.as_slice()).collect();
                write(&s, frames)
            }
        }
        .map_err(|e| fail(e.into()))?;
    }
    drop(p);
    let Some(mut o) = state else {
        return Err(fail(VideoError::NoSound(src.to_path_buf())));
    };
    if let Some(r) = o.resampler.take() {
        let (writer, peaks) = (&mut o.writer, &mut o.peaks);
        r.finish(&mut |s, n| {
            writer.write_planar(s, n)?;
            peaks.push(s, n);
            Ok(())
        })
        .map_err(|e| fail(e.into()))?;
    }
    let frames = o.writer.frames();
    let path = o.writer.finish().map_err(|e| fail(e.into()))?;
    let peaks = o.peaks.finish();
    let peaks_path = peaks_path_for(&path);
    peaks.save(&peaks_path).map_err(|e| fail(e.into()))?;
    progress
        .done
        .store(progress.total.load(Ordering::Relaxed), Ordering::Relaxed);
    Ok(ImportedAudio {
        original: src.to_path_buf(),
        name,
        path,
        peaks_path,
        channels: o.channels,
        frames,
        sample_rate: rate,
        original_rate: o.original_rate,
        peaks,
    })
}
