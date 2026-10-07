//! The flash-and-beep test: a clip that flashes white for one frame at
//! every whole second (a bar moving across shows it plays), and a sound
//! file beeping for that frame's length at the same moments. Played
//! together, a flash and its beep land at once when picture and sound are
//! in step; filmed with a phone at a high frame rate, the frames between
//! them measure what is left for the picture offset.

use crate::{Result, VideoError};
use gst::prelude::*;
use std::path::Path;

/// Write the test clip (`video`, MJPEG in Matroska) and its beeps
/// (`sound`, 48 kHz stereo float WAV), `seconds` long at `fps`.
pub fn make_sync_test(video: &Path, sound: &Path, seconds: u32, fps: u32) -> Result<()> {
    crate::init()?;
    let (w, h) = (640u32, 360u32);
    let desc = format!(
        "appsrc name=src format=time caps=video/x-raw,format=RGBA,width={w},height={h},framerate={fps}/1 ! videoconvert ! jpegenc quality=90 ! matroskamux ! filesink name=out"
    );
    let pipeline = gst::parse::launch(&desc)?
        .downcast::<gst::Pipeline>()
        .map_err(|_| VideoError::Gst("not a pipeline".into()))?;
    let out = pipeline
        .by_name("out")
        .ok_or_else(|| VideoError::Gst("no file sink".into()))?;
    out.set_property("location", video.to_string_lossy().as_ref());
    let src = pipeline
        .by_name("src")
        .and_then(|e| e.downcast::<gst_app::AppSrc>().ok())
        .ok_or_else(|| VideoError::Gst("no app source".into()))?;
    pipeline.set_state(gst::State::Playing)?;
    let frames = seconds * fps;
    let len = 1_000_000_000u64 / fps as u64;
    for n in 0..frames {
        let flash = n % fps == 0;
        let bar = (n % fps) * w / fps;
        let mut data = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let v = if flash {
                    255
                } else if y > h * 3 / 4 && y < h * 3 / 4 + 12 && x >= bar && x < bar + w / fps {
                    160
                } else {
                    0
                };
                data[i..i + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        let mut buf = gst::Buffer::from_mut_slice(data);
        if let Some(b) = buf.get_mut() {
            b.set_pts(gst::ClockTime::from_nseconds(n as u64 * len));
            b.set_duration(gst::ClockTime::from_nseconds(len));
        }
        src.push_buffer(buf)
            .map_err(|e| VideoError::Gst(format!("{e:?}")))?;
    }
    src.end_of_stream()
        .map_err(|e| VideoError::Gst(format!("{e:?}")))?;
    let bus = pipeline
        .bus()
        .ok_or_else(|| VideoError::Gst("no bus".into()))?;
    let msg = bus.timed_pop_filtered(
        gst::ClockTime::from_seconds(120),
        &[gst::MessageType::Eos, gst::MessageType::Error],
    );
    let _ = pipeline.set_state(gst::State::Null);
    match msg.as_ref().map(|m| m.view()) {
        Some(gst::MessageView::Eos(_)) => {}
        Some(gst::MessageView::Error(e)) => {
            return Err(VideoError::media(video, e.error().to_string()));
        }
        _ => return Err(VideoError::media(video, "timed out")),
    }
    // The beeps: 1 kHz for one frame at every second.
    let rate = 48_000u32;
    let total = (seconds * rate) as usize;
    let beep = (rate / fps) as usize;
    let tone: Vec<f32> = (0..total)
        .map(|i| {
            if i % rate as usize >= beep {
                0.0
            } else {
                (std::f32::consts::TAU * 1000.0 * i as f32 / rate as f32).sin() * 0.5
            }
        })
        .collect();
    faderframe_audio_files::write_wav(
        sound,
        &[tone.clone(), tone],
        rate,
        faderframe_audio_files::WavFormat::Float32,
        false,
    )?;
    Ok(())
}
