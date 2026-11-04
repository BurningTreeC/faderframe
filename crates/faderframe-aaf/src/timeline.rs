//! What an AAF composition says about sound, without AAF's machinery:
//! mono tracks of clips over media, fades, clip gain and its envelope,
//! the timecode the composition starts at, markers. [`crate::write`]
//! turns one into a file, [`crate::read`] reads one out of a file.
//!
//! Times are samples at the composition's rate.

/// A composition.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Composition {
    pub name: String,
    /// Samples per second of every position and length.
    pub rate: u32,
    /// The timecode at position 0.
    pub timecode: Option<Timecode>,
    /// Mono tracks (AAF's sound slots), in order.
    pub tracks: Vec<Track>,
    pub markers: Vec<Marker>,
    pub media: Vec<Media>,
}

/// A timecode start: `start` frames at `fps` (rounded: 30 for 29.97),
/// drop-frame or not; `rate` is the exact frame rate as a fraction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timecode {
    pub start: i64,
    pub fps: u16,
    pub drop: bool,
    pub rate: (i32, i32),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Track {
    pub name: String,
    pub clips: Vec<Clip>,
}

/// A piece of one channel of a medium, placed on a track.
#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    pub name: String,
    pub position: i64,
    pub length: i64,
    /// Index into [`Composition::media`].
    pub media: usize,
    /// The medium's channel it plays (0-based).
    pub channel: u16,
    /// Where in the medium it starts (samples at the composition rate).
    pub offset: i64,
    pub gain_db: f64,
    /// Level automation of the clip: points `(clip-relative position, dB)`
    /// joined by straight lines in amplitude (empty: none). Clip gain is
    /// not in it.
    pub envelope: Vec<(i64, f64)>,
    pub fade_in: Option<Fade>,
    pub fade_out: Option<Fade>,
}

impl Clip {
    pub fn end(&self) -> i64 {
        self.position + self.length
    }
}

/// A fade at a clip's edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fade {
    pub length: i64,
    pub curve: FadeCurve,
}

/// The shapes a fade can take (fade-in gain at fraction `t`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FadeCurve {
    #[default]
    Linear,
    EqualPower,
    SCurve,
    /// Slow start (cubic).
    Slow,
    /// Fast start (inverse cubic).
    Fast,
}

impl FadeCurve {
    pub const ALL: [FadeCurve; 5] = [
        FadeCurve::Linear,
        FadeCurve::EqualPower,
        FadeCurve::SCurve,
        FadeCurve::Slow,
        FadeCurve::Fast,
    ];

    pub fn gain(self, t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);
        match self {
            FadeCurve::Linear => t,
            FadeCurve::EqualPower => (t * std::f64::consts::FRAC_PI_2).sin(),
            FadeCurve::SCurve => t * t * (3.0 - 2.0 * t),
            FadeCurve::Slow => t * t * t,
            FadeCurve::Fast => 1.0 - (1.0 - t).powi(3),
        }
    }

    /// The shape nearest to a fade-in drawn through `points` (fraction,
    /// gain).
    pub fn nearest(points: &[(f64, f64)]) -> FadeCurve {
        let err =
            |c: FadeCurve| -> f64 { points.iter().map(|&(t, g)| (c.gain(t) - g).powi(2)).sum() };
        FadeCurve::ALL
            .into_iter()
            .min_by(|a, b| err(*a).total_cmp(&err(*b)))
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Marker {
    pub position: i64,
    pub name: String,
    /// Red, green, blue (0–255).
    pub color: Option<[u8; 3]>,
}

/// A medium: one recording of one or more channels.
#[derive(Clone, Debug, PartialEq)]
pub struct Media {
    pub name: String,
    pub channels: u16,
    /// Its sample rate.
    pub rate: u32,
    /// Its length in frames at its rate.
    pub frames: i64,
    /// Where each channel's sound is.
    pub essence: Vec<Essence>,
}

/// Where a channel's sound is.
#[derive(Clone, Debug, PartialEq)]
pub enum Essence {
    /// Writing: PCM of this many bits embedded in the file, filled from the
    /// writer's [`crate::write::Samples`].
    Embed { bits: u16 },
    /// Writing: in a file beside the AAF (its URL, and the header of the
    /// WAVE file for the descriptor).
    Link { url: String, wave_header: Vec<u8> },
    /// Read: embedded in the file.
    Embedded(Embedded),
    /// Read: in a file the composition points at, channel `channel` of it.
    Linked {
        urls: Vec<String>,
        channel: u16,
        format: EssenceFormat,
    },
    /// Read: neither embedded nor located (offline media).
    Missing,
}

/// Embedded essence of a file being read.
#[derive(Clone, Debug, PartialEq)]
pub struct Embedded {
    /// The essence stream's compound-file entry.
    pub entry: usize,
    pub len: u64,
    pub format: EssenceFormat,
    /// The channel of the stream this medium's channel is.
    pub channel: u16,
}

/// What the bytes of an essence stream or file are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EssenceFormat {
    /// Raw interleaved PCM.
    Pcm {
        bits: u16,
        channels: u16,
        rate: u32,
        big_endian: bool,
        float: bool,
    },
    /// A RIFF WAVE file.
    Wave,
    /// An AIFF or AIFC file.
    Aiff,
}
