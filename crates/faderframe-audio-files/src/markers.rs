//! Loop points and markers in WAV files, as samplers and game audio
//! middleware read them: the `smpl` chunk (sample loops: a forward loop
//! from its first to its last frame, played again and again) and the
//! `cue ` chunk with its `LIST`/`adtl` labels (named markers on frames).
//!
//! Frames count from the file's first frame; a loop's `end` here is the
//! frame after its last (the chunk stores the last frame itself).

use std::io;
use std::path::Path;

/// A forward loop over frames `[start, end)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WavLoop {
    pub start: u32,
    pub end: u32,
}

/// A named marker on a frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WavCue {
    pub frame: u32,
    pub label: String,
}

/// Loops and markers of a file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WavMarkers {
    pub loops: Vec<WavLoop>,
    pub cues: Vec<WavCue>,
}

impl WavMarkers {
    pub fn is_empty(&self) -> bool {
        self.loops.is_empty() && self.cues.is_empty()
    }

    /// The `smpl`, `cue ` and `LIST`/`adtl` chunks (whole, padded) for a
    /// file at `sample_rate` — what [`crate::wav::write_wav_chunks`]
    /// appends after the audio.
    pub fn chunks(&self, sample_rate: u32) -> Vec<u8> {
        let mut out = Vec::new();
        if !self.loops.is_empty() {
            let mut c = Vec::with_capacity(36 + 24 * self.loops.len());
            let period = (1e9 / f64::from(sample_rate.max(1))).round() as u32;
            for v in [0u32, 0, period, 60, 0, 0, 0, self.loops.len() as u32, 0] {
                c.extend_from_slice(&v.to_le_bytes());
            }
            for (i, l) in self.loops.iter().enumerate() {
                let last = l.end.max(l.start + 1) - 1;
                // Id, forward, first frame, last frame, no fraction, for ever.
                for v in [i as u32, 0, l.start, last, 0, 0] {
                    c.extend_from_slice(&v.to_le_bytes());
                }
            }
            chunk(&mut out, b"smpl", &c);
        }
        if !self.cues.is_empty() {
            let mut c = Vec::with_capacity(4 + 24 * self.cues.len());
            c.extend_from_slice(&(self.cues.len() as u32).to_le_bytes());
            for (i, cue) in self.cues.iter().enumerate() {
                c.extend_from_slice(&(i as u32 + 1).to_le_bytes());
                c.extend_from_slice(&cue.frame.to_le_bytes());
                c.extend_from_slice(b"data");
                c.extend_from_slice(&0u32.to_le_bytes());
                c.extend_from_slice(&0u32.to_le_bytes());
                c.extend_from_slice(&cue.frame.to_le_bytes());
            }
            chunk(&mut out, b"cue ", &c);
            let mut list = Vec::new();
            list.extend_from_slice(b"adtl");
            for (i, cue) in self.cues.iter().enumerate() {
                let mut l = Vec::with_capacity(5 + cue.label.len());
                l.extend_from_slice(&(i as u32 + 1).to_le_bytes());
                l.extend_from_slice(cue.label.as_bytes());
                l.push(0);
                chunk(&mut list, b"labl", &l);
            }
            chunk(&mut out, b"LIST", &list);
        }
        out
    }

    /// Read the loops and markers of a WAV file (none when it has none).
    pub fn read(path: &Path) -> io::Result<WavMarkers> {
        let mut m = WavMarkers::default();
        if let Some(smpl) = crate::wavstream::read_chunk(path, b"smpl")? {
            let n = u32_at(&smpl, 28).unwrap_or(0) as usize;
            for i in 0..n {
                let at = 36 + 24 * i;
                let (Some(start), Some(last)) = (u32_at(&smpl, at + 8), u32_at(&smpl, at + 12))
                else {
                    break;
                };
                m.loops.push(WavLoop {
                    start,
                    end: last + 1,
                });
            }
        }
        let mut labels: Vec<(u32, String)> = Vec::new();
        if let Some(list) = crate::wavstream::read_chunk(path, b"LIST")?
            && list.get(0..4) == Some(b"adtl")
        {
            let mut at = 4;
            while at + 8 <= list.len() {
                let id = &list[at..at + 4];
                let size = u32_at(&list, at + 4).unwrap_or(0) as usize;
                let body = list.get(at + 8..at + 8 + size).unwrap_or(&[]);
                if id == b"labl" && body.len() >= 4 {
                    let cue = u32_at(body, 0).unwrap_or(0);
                    let text = body[4..].split(|b| *b == 0).next().unwrap_or(&[]);
                    labels.push((cue, String::from_utf8_lossy(text).into_owned()));
                }
                at += 8 + size + (size & 1);
            }
        }
        if let Some(cue) = crate::wavstream::read_chunk(path, b"cue ")? {
            let n = u32_at(&cue, 0).unwrap_or(0) as usize;
            for i in 0..n {
                let at = 4 + 24 * i;
                let (Some(id), Some(frame)) = (u32_at(&cue, at), u32_at(&cue, at + 20)) else {
                    break;
                };
                let label = labels
                    .iter()
                    .find(|(c, _)| *c == id)
                    .map(|(_, l)| l.clone())
                    .unwrap_or_default();
                m.cues.push(WavCue { frame, label });
            }
        }
        Ok(m)
    }
}

fn chunk(out: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at + 4)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::{Dither, WavFormat, read_wav};

    #[test]
    fn loops_and_markers_are_written_and_read_back_beside_the_audio() {
        let dir = std::env::temp_dir().join(format!("ff-markers-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("loop.wav");
        let audio: Vec<Vec<f32>> = (0..2)
            .map(|c| (0..1001).map(|i| ((i + c) % 7) as f32 / 8.0).collect())
            .collect();
        let markers = WavMarkers {
            loops: vec![WavLoop {
                start: 100,
                end: 900,
            }],
            cues: vec![
                WavCue {
                    frame: 100,
                    label: "Entry".into(),
                },
                WavCue {
                    frame: 900,
                    label: "Exit".into(),
                },
                WavCue {
                    frame: 450,
                    label: "Hit".into(),
                },
            ],
        };
        crate::wav::write_wav_chunks(
            &path,
            &audio,
            48_000,
            WavFormat::Pcm24,
            Dither::Off,
            None,
            &markers.chunks(48_000),
        )
        .unwrap();
        // The audio as ever (an odd length: the pad byte before the chunks).
        let wav = read_wav(&path).unwrap();
        assert_eq!(wav.channels[0].len(), 1001);
        assert!((wav.channels[1][3] - 4.0 / 8.0).abs() < 1e-6);
        assert_eq!(WavMarkers::read(&path).unwrap(), markers);
        // The chunk holds the loop's last frame.
        let smpl = crate::wavstream::read_chunk(&path, b"smpl")
            .unwrap()
            .unwrap();
        assert_eq!(u32_at(&smpl, 36 + 12), Some(899));
        assert_eq!(u32_at(&smpl, 8), Some(20_833), "ns a sample");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
