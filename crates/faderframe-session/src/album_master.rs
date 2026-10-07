//! The album as one continuous stream ([`Assembler`]): songs after a pause
//! or overlapping the previous one in an equal-power crossfade, each track
//! mark on a CD-frame boundary (1/75 s). What comes out goes to the album
//! file, the CD master ([`CdMaster`]: resampled to 44.1 kHz, dithered to 16
//! bits, written as a DDP 2.00 fileset) and — when songs overlap — the
//! song files, cut at the marks so they play gaplessly ([`Gapless`]).
//! [`disc`] describes the stream as a CD (PQ codes, CD-Text) for the DDP
//! fileset and the cue sheet.

use faderframe_audio_files::Dither;
use faderframe_audio_files::WavFormat;
use faderframe_audio_files::dither::Quantizer;
use faderframe_audio_files::resample::StreamResampler;
use faderframe_audio_files::wavstream::WavWriter;
use faderframe_disc::ddp::{DdpNames, DdpWriter, Fileset};
use faderframe_disc::{CD_RATE, CdText, Disc, Language, MIN_PREGAP, TextBlock, Track, TrackFlags};
use faderframe_project::album::{Album, Credits};
use std::path::{Path, PathBuf};

/// Where a song sits in the stream (frames at the album's rate).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Mark {
    /// The pause before the song starts here (CD index 00).
    pub pause: Option<u64>,
    /// The song (index 01).
    pub start: u64,
}

/// Frames per CD frame at `rate` (1 when the rate has no whole number).
pub(crate) fn cd_frame(rate: u32) -> u64 {
    if rate.is_multiple_of(75) {
        u64::from(rate / 75)
    } else {
        1
    }
}

/// Sends flushed stream frames on: (the marks so far, stream position,
/// stereo planes).
pub(crate) type Sink<'a> = dyn FnMut(&[Mark], u64, &[&[f32]]) -> Result<(), String> + 'a;

/// Builds the album stream song by song, holding back what a following
/// crossfade will overlap.
pub(crate) struct Assembler {
    rate: u32,
    frame: u64,
    /// Frames flushed so far.
    flushed: u64,
    /// Frames after `flushed`, not yet flushed.
    hold: [Vec<f32>; 2],
    pub marks: Vec<Mark>,
}

impl Assembler {
    pub fn new(rate: u32) -> Self {
        Self {
            rate,
            frame: cd_frame(rate),
            flushed: 0,
            hold: [Vec::new(), Vec::new()],
            marks: Vec::new(),
        }
    }

    fn frames(&self, seconds: f32) -> u64 {
        (f64::from(seconds.max(0.0)) * f64::from(self.rate)).round() as u64
    }

    /// The stream's length so far.
    fn end(&self) -> u64 {
        self.flushed + self.hold[0].len() as u64
    }

    /// Add a song (stereo): after `pause` seconds, or overlapping the
    /// previous song by `crossfade` seconds. `keep` seconds of its end are
    /// held back for the next song's crossfade.
    pub fn add(
        &mut self,
        audio: &[Vec<f32>],
        pause: f32,
        crossfade: f32,
        keep: f32,
        sink: &mut Sink<'_>,
    ) -> Result<(), String> {
        let len = audio[0].len() as u64;
        let end = self.end();
        let mark = if self.marks.is_empty() {
            Mark {
                pause: None,
                start: end,
            }
        } else if crossfade > 0.0 {
            let c = self
                .frames(crossfade)
                .min(self.hold[0].len() as u64)
                .min(len);
            // The mark on a CD frame at or before the overlap's start, never
            // before what was flushed already.
            let start = ((end - c) / self.frame * self.frame).max(self.flushed);
            Mark { pause: None, start }
        } else {
            let wanted = end + self.frames(pause);
            let start = wanted.div_ceil(self.frame) * self.frame;
            Mark {
                pause: (start > end).then_some(end),
                start,
            }
        };
        if mark.start >= end {
            for ch in &mut self.hold {
                ch.resize(ch.len() + (mark.start - end) as usize, 0.0);
            }
            for (c, ch) in self.hold.iter_mut().enumerate() {
                ch.extend_from_slice(&audio[c.min(audio.len() - 1)]);
            }
        } else {
            // Equal-power crossfade over [start, end).
            let at = (mark.start - self.flushed) as usize;
            let overlap = ((end - mark.start) as usize).min(len as usize);
            for (c, ch) in self.hold.iter_mut().enumerate() {
                let src = &audio[c.min(audio.len() - 1)];
                for k in 0..overlap {
                    let x = (k as f64 + 0.5) / overlap as f64 * std::f64::consts::FRAC_PI_2;
                    let s = &mut ch[at + k];
                    *s = (f64::from(*s) * x.cos() + f64::from(src[k]) * x.sin()) as f32;
                }
                ch.extend_from_slice(&src[overlap..]);
            }
        }
        self.marks.push(mark);
        let keep = (self.frames(keep) + self.frame) as usize;
        let flush = self.hold[0].len().saturating_sub(keep);
        self.flush(flush, sink)
    }

    fn flush(&mut self, frames: usize, sink: &mut Sink<'_>) -> Result<(), String> {
        if frames == 0 {
            return Ok(());
        }
        sink(
            &self.marks,
            self.flushed,
            &[&self.hold[0][..frames], &self.hold[1][..frames]],
        )?;
        for ch in &mut self.hold {
            ch.drain(..frames);
        }
        self.flushed += frames as u64;
        Ok(())
    }

    /// Flush the rest; returns the marks and the stream's length.
    pub fn finish(mut self, sink: &mut Sink<'_>) -> Result<(Vec<Mark>, u64), String> {
        let rest = self.hold[0].len();
        self.flush(rest, sink)?;
        Ok((self.marks, self.flushed))
    }
}

/// The song files cut from the stream at the marks (gapless: each file
/// runs to the next song's mark, pauses included).
pub(crate) struct Gapless {
    paths: Vec<PathBuf>,
    rate: u32,
    format: WavFormat,
    dither: Dither,
    current: Option<(usize, WavWriter)>,
}

impl Gapless {
    pub fn new(paths: Vec<PathBuf>, rate: u32, format: WavFormat, dither: Dither) -> Self {
        Self {
            paths,
            rate,
            format,
            dither,
            current: None,
        }
    }

    /// Write stream frames from `pos`; `marks` holds every mark up to the
    /// frames' song.
    pub fn write(&mut self, marks: &[Mark], pos: u64, planes: &[&[f32]]) -> Result<(), String> {
        let n = planes[0].len() as u64;
        let mut done = 0u64;
        while done < n {
            let at = pos + done;
            let song = marks.iter().rposition(|m| m.start <= at).unwrap_or(0);
            let until = marks
                .get(song + 1)
                .map_or(u64::MAX, |m| m.start)
                .min(pos + n);
            if self.current.as_ref().is_none_or(|(s, _)| *s != song) {
                if let Some((_, w)) = self.current.take() {
                    w.finish().map_err(|e| e.to_string())?;
                }
                let path = &self.paths[song];
                let w = WavWriter::create_with(path, 2, self.rate, self.format, self.dither)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                self.current = Some((song, w));
            }
            let k = (until - at) as usize;
            let a = done as usize;
            if let Some((_, w)) = self.current.as_mut() {
                w.write_planar(&[&planes[0][a..a + k], &planes[1][a..a + k]], k)
                    .map_err(|e| e.to_string())?;
            }
            done += k as u64;
        }
        Ok(())
    }

    pub fn finish(self) -> Result<(), String> {
        if let Some((_, w)) = self.current {
            w.finish().map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

/// The CD master: the stream at 44.1 kHz and 16 bits into a DDP fileset,
/// after track 1's 2-second pregap.
pub(crate) struct CdMaster {
    writer: DdpWriter,
    resampler: Option<StreamResampler>,
    quantizer: Quantizer,
    samples: Vec<i16>,
}

impl CdMaster {
    pub fn create(dir: &Path, rate: u32, dither: Dither) -> Result<Self, String> {
        let mut writer = DdpWriter::create(dir, DdpNames::default()).map_err(|e| e.to_string())?;
        writer
            .write_silence(u64::from(MIN_PREGAP) * faderframe_disc::SECTOR_FRAMES)
            .map_err(|e| e.to_string())?;
        let resampler = if rate == CD_RATE {
            None
        } else {
            Some(StreamResampler::new(rate, CD_RATE, 2).map_err(|e| e.to_string())?)
        };
        Ok(Self {
            writer,
            resampler,
            quantizer: Quantizer::new(16, 2, CD_RATE, dither),
            samples: Vec::new(),
        })
    }

    fn quantize(
        writer: &mut DdpWriter,
        quantizer: &mut Quantizer,
        samples: &mut Vec<i16>,
        planes: &[&[f32]],
        frames: usize,
    ) -> std::io::Result<()> {
        samples.clear();
        for i in 0..frames {
            for (c, p) in planes.iter().enumerate().take(2) {
                samples.push(quantizer.quantize(c, p[i]) as i16);
            }
        }
        writer.write_interleaved(samples)
    }

    pub fn write(&mut self, planes: &[&[f32]]) -> Result<(), String> {
        let frames = planes[0].len();
        let Self {
            writer,
            resampler,
            quantizer,
            samples,
        } = self;
        match resampler {
            None => Self::quantize(writer, quantizer, samples, planes, frames)
                .map_err(|e| e.to_string()),
            Some(r) => {
                let input = [planes[0].to_vec(), planes[1].to_vec()];
                r.push(&input, frames, &mut |p, n| {
                    Self::quantize(writer, quantizer, samples, p, n).map_err(Into::into)
                })
                .map_err(|e| e.to_string())
            }
        }
    }

    pub fn finish(mut self, disc: &Disc) -> Result<Fileset, String> {
        if let Some(r) = self.resampler.take() {
            let Self {
                writer,
                quantizer,
                samples,
                ..
            } = &mut self;
            r.finish(&mut |p, n| {
                Self::quantize(writer, quantizer, samples, p, n).map_err(Into::into)
            })
            .map_err(|e| e.to_string())?;
        }
        self.writer.finish(disc).map_err(|e| e.to_string())
    }
}

fn cd_text(title: &str, credits: &Credits) -> CdText {
    CdText {
        title: title.to_string(),
        performer: credits.performer.clone(),
        songwriter: credits.songwriter.clone(),
        composer: credits.composer.clone(),
        arranger: credits.arranger.clone(),
        message: credits.message.clone(),
    }
}

/// The album as a disc: track marks in CD frames from the start of the
/// stream plus `offset` (the CD master's 2-second pregap; 0 for the album
/// file's cue sheet), codes and — with `text` — the titles and credits.
/// The album's title is `title` when it has none of its own.
pub(crate) fn disc(
    album: &Album,
    title: &str,
    marks: &[Mark],
    (rate, length): (u32, u64),
    offset: u32,
    text: bool,
) -> Disc {
    let frame = cd_frame(rate);
    let to_cd = |frames: u64| -> u32 {
        if frame > 1 {
            (frames / frame) as u32
        } else {
            (frames * 75 / u64::from(rate.max(1))) as u32
        }
    };
    let tracks = album
        .songs
        .iter()
        .zip(marks)
        .enumerate()
        .map(|(i, (song, mark))| Track {
            pregap: if i == 0 {
                (offset > 0).then_some(0)
            } else {
                mark.pause.map(|p| to_cd(p) + offset)
            },
            indexes: vec![to_cd(mark.start) + offset],
            isrc: faderframe_disc::normalize_isrc(&song.isrc).ok(),
            flags: TrackFlags {
                copy_permitted: album.settings.copy_permitted,
                ..TrackFlags::default()
            },
            text: if text {
                cd_text(&song.title, &song.credits)
            } else {
                CdText::default()
            },
        })
        .collect();
    let album_title = if album.info.title.is_empty() {
        title
    } else {
        &album.info.title
    };
    // Further languages: each field the translation leaves empty is the
    // main text's.
    let more_text = if text {
        album
            .info
            .translations
            .iter()
            .map(|tr| TextBlock {
                language: Language(tr.language),
                disc: cd_text(
                    if tr.title.is_empty() {
                        album_title
                    } else {
                        &tr.title
                    },
                    &faderframe_project::album::fall_back(&tr.credits, &album.info.credits),
                ),
                tracks: album
                    .songs
                    .iter()
                    .map(|song| {
                        let own = tr.song(song.id);
                        let title = own
                            .map(|t| t.title.as_str())
                            .filter(|t| !t.is_empty())
                            .unwrap_or(&song.title);
                        let credits = own.map_or_else(
                            || song.credits.clone(),
                            |t| faderframe_project::album::fall_back(&t.credits, &song.credits),
                        );
                        cd_text(title, &credits)
                    })
                    .collect(),
            })
            .collect()
    } else {
        Vec::new()
    };
    Disc {
        upc: faderframe_disc::normalize_upc(&album.info.upc).ok(),
        master_id: album_title.chars().take(48).collect(),
        text: if text {
            cd_text(album_title, &album.info.credits)
        } else {
            CdText::default()
        },
        text_language: Language(album.info.language),
        more_text,
        tracks,
        sectors: to_cd(length) + offset,
    }
}

/// The album's codes, checked before anything is rendered.
pub(crate) fn check_codes(album: &Album) -> Result<(), String> {
    if !album.info.upc.trim().is_empty() {
        faderframe_disc::normalize_upc(&album.info.upc).map_err(|e| e.to_string())?;
    }
    for s in &album.songs {
        if !s.isrc.trim().is_empty() {
            faderframe_disc::normalize_isrc(&s.isrc).map_err(|e| format!("{}: {e}", s.title))?;
        }
    }
    if album.settings.ddp && album.songs.len() > 99 {
        return Err(format!(
            "a CD holds 99 tracks, the album has {}",
            album.songs.len()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frames: usize, v: f32) -> Vec<Vec<f32>> {
        vec![vec![v; frames]; 2]
    }

    /// Assemble songs (frames, pause s, crossfade s) and collect the stream.
    fn assemble(rate: u32, songs: &[(usize, f32, f32)]) -> (Vec<f32>, Vec<Mark>) {
        let mut a = Assembler::new(rate);
        let mut out = Vec::new();
        let mut sink = |_: &[Mark], pos: u64, p: &[&[f32]]| {
            assert_eq!(pos as usize, out.len(), "contiguous");
            out.extend_from_slice(p[0]);
            Ok(())
        };
        for (i, &(n, pause, xf)) in songs.iter().enumerate() {
            let keep = songs.get(i + 1).map_or(0.0, |s| s.2);
            a.add(&tone(n, 1.0), pause, xf, keep, &mut sink).unwrap();
        }
        let (marks, len) = a.finish(&mut sink).unwrap();
        assert_eq!(len as usize, out.len());
        (out, marks)
    }

    #[test]
    fn pauses_and_crossfades_put_marks_on_cd_frames() {
        // 44.1 kHz: CD frames of 588. A pause rounds up, so the second
        // song's mark lands on a frame; silence fills the pause.
        let (out, marks) = assemble(44_100, &[(10_000, 0.0, 0.0), (10_000, 0.5, 0.0)]);
        assert_eq!(
            marks[0],
            Mark {
                pause: None,
                start: 0
            }
        );
        let m = marks[1];
        assert_eq!(m.pause, Some(10_000));
        assert_eq!(m.start % 588, 0);
        assert!(m.start >= 10_000 + 22_050 && m.start < 10_000 + 22_050 + 588);
        assert!(out[10_000..m.start as usize].iter().all(|v| *v == 0.0));
        assert_eq!(out.len() as u64, m.start + 10_000);
        // A crossfade overlaps: the mark at or before the overlap's start,
        // equal-power sums of two full-scale signals peak at √2.
        let (out, marks) = assemble(48_000, &[(48_000, 0.0, 0.0), (48_000, 0.0, 0.25)]);
        let m = marks[1];
        assert_eq!(m.pause, None);
        assert_eq!(m.start % 640, 0);
        assert!(m.start <= 36_000 && m.start > 36_000 - 640, "{}", m.start);
        assert_eq!(out.len() as u64, m.start + 48_000);
        let mid = (m.start as usize + 48_000) / 2;
        assert!(
            (out[mid] - std::f32::consts::SQRT_2).abs() < 0.01,
            "{}",
            out[mid]
        );
        assert!((out[(m.start - 1) as usize] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn the_disc_follows_the_marks() {
        use faderframe_core::SongId;
        use faderframe_project::album::{Song, SongSource};
        let mut album = Album::default();
        for i in 0..2 {
            let mut s = Song::new(SongId(i), format!("Song {i}"), SongSource::ThisProject);
            s.isrc = format!("us-abc-26-0000{i}");
            album.songs.push(s);
        }
        album.info.upc = "036000291452".into();
        let marks = [
            Mark {
                pause: None,
                start: 0,
            },
            Mark {
                pause: Some(44_100 * 300),
                start: 44_100 * 302,
            },
        ];
        let d = disc(
            &album,
            "Test",
            &marks,
            (44_100, 44_100 * 600),
            MIN_PREGAP,
            true,
        );
        assert_eq!(d.upc.as_deref(), Some("0036000291452"));
        assert_eq!(d.tracks[0].pregap, Some(0));
        assert_eq!(d.tracks[0].indexes, vec![150]);
        assert_eq!(d.tracks[1].pregap, Some(150 + 300 * 75));
        assert_eq!(d.tracks[1].indexes, vec![150 + 302 * 75]);
        assert_eq!(d.tracks[1].isrc.as_deref(), Some("USABC2600001"));
        assert_eq!(d.text.title, "Test");
        assert_eq!(d.sectors, 150 + 600 * 75);
        assert_eq!(d.validate(), Ok(()));
        assert!(check_codes(&album).is_ok());
        album.songs[1].isrc = "nonsense".into();
        assert!(check_codes(&album).is_err());
    }
}
