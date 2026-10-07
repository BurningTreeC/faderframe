//! Object-based masters: the Audio Definition Model (ITU-R BS.2076) that
//! an ADM BWF file carries in its `axml` and `chna` chunks (ITU-R BS.2088),
//! written to Dolby's Atmos master ADM profile (v1.1) and read back from
//! any ADM file.
//!
//! A master is a bed — one of the profile's eight channel beds, 2.0 up to
//! 7.1.2, its channels at fixed places of the room — and up to 118 objects,
//! each one channel with its place over time in blocks. Places are the
//! profile's Cartesian room: `x` −1 left … 1 right, `y` −1 back … 1 front,
//! `z` 0 ear level … 1 ceiling (the panner's coordinates in
//! `faderframe_core::surround`). The file's channels are the bed's, then
//! the objects'.
//!
//! Profile rules followed (as MediaArea's conformance checker states them):
//! one programme `APR_1001` named `Atmos_Master`; contents `ACO_1001…` in
//! order, each with `dialogue` 2 (mixed) and one object; the bed object
//! `AO_1001`, objects `AO_100b`…`AO_1080`; custom IDs (`xxxx` from `1001`),
//! track formats `_01`; bed channels Dolby's (`RoomCentricLeft`, `RC_L`, …)
//! with Cartesian positions in one block without times; object blocks with
//! `rtime` and `duration`, Cartesian positions, equal width/depth/height,
//! a gain and `jumpPosition` 1 with an interpolation of 0.005208 s (0 on the
//! first block); track UIDs referring to track and pack formats; 48 or
//! 96 kHz, 24 bit, at most 128 channels. Not written: Dolby's `dbmd` chunk
//! (trim and downmix metadata, binaural render modes).

#![forbid(unsafe_code)]

mod read;
mod write;

pub use read::{ReadError, Scene, SceneBlock, SceneObject, SceneSpeaker, parse};
pub use write::{AdmError, axml, chna, validate};

/// Objects a master may have (the bed's object is `AO_1001`, objects run
/// from `AO_100b` to `AO_1080`).
pub const MAX_OBJECTS: usize = 118;
/// Channels a master may have.
pub const MAX_CHANNELS: usize = 128;
/// The programme name that marks a Dolby Atmos master.
pub const PROGRAMME: &str = "Atmos_Master";
/// The interpolation of every object block but the first, seconds.
pub const INTERPOLATION: f64 = 0.005208;

/// A bed channel of the profile, at its fixed place in the room.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BedChannel {
    L,
    R,
    C,
    Lfe,
    /// Side surrounds (7.x).
    Lss,
    Rss,
    /// Rear surrounds (7.x).
    Lrs,
    Rrs,
    /// Top surrounds (x.x.2, above the listener).
    Lts,
    Rts,
    /// The 5.x surrounds.
    Ls,
    Rs,
}

use BedChannel::*;

impl BedChannel {
    pub const ALL: [BedChannel; 12] = [L, R, C, Lfe, Lss, Rss, Lrs, Rrs, Lts, Rts, Ls, Rs];

    /// Its channel format name.
    pub fn name(self) -> &'static str {
        match self {
            L => "RoomCentricLeft",
            R => "RoomCentricRight",
            C => "RoomCentricCenter",
            Lfe => "RoomCentricLFE",
            Lss => "RoomCentricLeftSideSurround",
            Rss => "RoomCentricRightSideSurround",
            Lrs => "RoomCentricLeftRearSurround",
            Rrs => "RoomCentricRightRearSurround",
            Lts => "RoomCentricLeftTopSurround",
            Rts => "RoomCentricRightTopSurround",
            Ls => "RoomCentricLeftSurround",
            Rs => "RoomCentricRightSurround",
        }
    }

    /// Its speaker label.
    pub fn label(self) -> &'static str {
        match self {
            L => "RC_L",
            R => "RC_R",
            C => "RC_C",
            Lfe => "RC_LFE",
            Lss => "RC_Lss",
            Rss => "RC_Rss",
            Lrs => "RC_Lrs",
            Rrs => "RC_Rrs",
            Lts => "RC_Lts",
            Rts => "RC_Rts",
            Ls => "RC_Ls",
            Rs => "RC_Rs",
        }
    }

    /// The ITU-R BS.2051 label of the same speaker.
    pub fn itu(self) -> &'static str {
        match self {
            L => "M+030",
            R => "M-030",
            C => "M+000",
            Lfe => "LFE1",
            Lss => "M+090",
            Rss => "M-090",
            Lrs => "M+135",
            Rrs => "M-135",
            Lts => "U+090",
            Rts => "U-090",
            Ls => "M+110",
            Rs => "M-110",
        }
    }

    /// Its place `[x, y, z]` (the LFE's is the profile's nominal one).
    pub fn position(self) -> [f32; 3] {
        match self {
            L => [-1.0, 1.0, 0.0],
            R => [1.0, 1.0, 0.0],
            C => [0.0, 1.0, 0.0],
            Lfe => [-1.0, 1.0, -1.0],
            Lss => [-1.0, 0.0, 0.0],
            Rss => [1.0, 0.0, 0.0],
            Lrs => [-1.0, -1.0, 0.0],
            Rrs => [1.0, -1.0, 0.0],
            Lts => [-1.0, 0.0, 1.0],
            Rts => [1.0, 0.0, 1.0],
            Ls => [-1.0, -0.36397, 0.0],
            Rs => [1.0, -0.36397, 0.0],
        }
    }

    pub fn is_lfe(self) -> bool {
        self == Lfe
    }

    /// The channel a speaker label names (the profile's or BS.2051's,
    /// with or without the `urn:itu:bs:2051:…:speaker:` prefix).
    pub fn from_label(label: &str) -> Option<BedChannel> {
        let label = label.rsplit(':').next().unwrap_or(label).trim();
        Self::ALL.into_iter().find(|c| {
            c.label() == label || c.itu() == label || (c.is_lfe() && label.starts_with("LFE"))
        })
    }
}

/// The profile's beds, each in its channel order.
pub const BEDS: [&[BedChannel]; 8] = [
    &[L, R],
    &[L, R, C],
    &[L, R, C, Ls, Rs],
    &[L, R, C, Lfe, Ls, Rs],
    &[L, R, C, Lss, Rss, Lrs, Rrs],
    &[L, R, C, Lfe, Lss, Rss, Lrs, Rrs],
    &[L, R, C, Lss, Rss, Lrs, Rrs, Lts, Rts],
    &[L, R, C, Lfe, Lss, Rss, Lrs, Rrs, Lts, Rts],
];

/// One stretch of an object's metadata: its place from `start` (frames from
/// the start of the file) for `length` frames.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Block {
    pub start: u64,
    pub length: u64,
    pub position: [f32; 3],
    /// Its extent, 0 … 1 (written as equal width, depth and height).
    pub size: f32,
    pub gain: f32,
}

/// An object: its name and its blocks, in order, covering the master.
#[derive(Clone, Debug, PartialEq)]
pub struct Object {
    pub name: String,
    pub blocks: Vec<Block>,
}

/// Which reading of ADM a file is written for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Profile {
    /// Dolby's Atmos master ADM profile: the programme `Atmos_Master`,
    /// stream formats naming their pack too, the LFE known by its label.
    #[default]
    DolbyAtmos,
    /// ITU-R BS.2076 as the EBU ADM renderer reads it: stream formats
    /// naming only their channel, the LFE marked by a 120 Hz low-pass, the
    /// programme named after the master.
    Itu,
}

/// An object-based master to write: the file has `bed.len()` channels for
/// the bed, then one per object.
#[derive(Clone, Debug, PartialEq)]
pub struct Master {
    /// The programme's name (an ITU master's; a Dolby master's is
    /// [`PROGRAMME`]).
    pub name: String,
    pub profile: Profile,
    pub sample_rate: u32,
    pub frames: u64,
    /// One of [`BEDS`] (or empty: objects only).
    pub bed: Vec<BedChannel>,
    pub objects: Vec<Object>,
}

impl Master {
    pub fn channels(&self) -> usize {
        self.bed.len() + self.objects.len()
    }
}

/// `hh:mm:ss.fffff` for a time in units of 10 µs.
pub(crate) fn time(units: u64) -> String {
    let s = units / 100_000;
    format!(
        "{:02}:{:02}:{:02}.{:05}",
        s / 3600,
        (s / 60) % 60,
        s % 60,
        units % 100_000
    )
}

/// Frames at `rate` in units of 10 µs (rounded: block times stay
/// contiguous because both ends of a block round the same way).
pub(crate) fn units(frames: u64, rate: u32) -> u64 {
    ((u128::from(frames) * 100_000 + u128::from(rate) / 2) / u128::from(rate.max(1))) as u64
}

/// Seconds of an ADM time: `hh:mm:ss.fffff…`, or `hh:mm:ss.fffffSrrrrr`
/// (samples at a rate).
pub(crate) fn seconds(t: &str) -> Option<f64> {
    let (hms, frac) = t.trim().split_once('.').unwrap_or((t.trim(), "0"));
    let mut parts = hms.split(':');
    let h: f64 = parts.next()?.parse().ok()?;
    let m: f64 = parts.next()?.parse().ok()?;
    let s: f64 = parts.next()?.parse().ok()?;
    let frac = match frac.split_once('S') {
        Some((samples, rate)) => samples.parse::<f64>().ok()? / rate.parse::<f64>().ok()?,
        None => format!("0.{frac}").parse::<f64>().ok()?,
    };
    Some(h * 3600.0 + m * 60.0 + s + frac)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_written_in_ten_microseconds() {
        assert_eq!(time(0), "00:00:00.00000");
        assert_eq!(
            time(units(48_000 * 3725 + 24_000, 48_000)),
            "01:02:05.50000"
        );
        assert_eq!(seconds("01:02:05.50000"), Some(3725.5));
        assert_eq!(seconds("00:00:01.24000S48000"), Some(1.5));
        // Neighbouring blocks share their rounded boundary.
        let (a, b) = (units(1001, 48_000), units(2003, 48_000));
        assert_eq!(a + (b - a), b);
    }

    #[test]
    fn labels_name_the_channels() {
        assert_eq!(BedChannel::from_label("RC_Lts"), Some(Lts));
        assert_eq!(
            BedChannel::from_label("urn:itu:bs:2051:0:speaker:M-110"),
            Some(Rs)
        );
        assert_eq!(BedChannel::from_label("LFE2"), Some(Lfe));
        assert_eq!(BedChannel::from_label("B+000"), None);
        assert!(
            BEDS.iter()
                .all(|b| b.iter().filter(|c| c.is_lfe()).count() <= 1)
        );
    }
}
