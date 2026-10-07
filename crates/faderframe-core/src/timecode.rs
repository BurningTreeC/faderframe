//! SMPTE frame rates and timecode for picture: every film and video rate
//! (the 1000/1001 rates as exact fractions), drop-frame numbering for 29.97
//! and 59.94, and the rate of a video file matched to its nearest SMPTE
//! rate.

use serde::{Deserialize, Serialize};

/// A frame rate of picture and timecode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FrameRate {
    /// 23.976 (24000/1001): film transferred to NTSC video, HD.
    #[serde(rename = "23.976")]
    Fps23976,
    #[serde(rename = "24")]
    Fps24,
    #[default]
    #[serde(rename = "25")]
    Fps25,
    /// 29.97 (30000/1001), non-drop numbering.
    #[serde(rename = "29.97")]
    Fps2997,
    /// 29.97 drop-frame: two labels skipped every minute but every tenth.
    #[serde(rename = "29.97df")]
    Fps2997Drop,
    #[serde(rename = "30")]
    Fps30,
    /// 47.952 (48000/1001).
    #[serde(rename = "47.952")]
    Fps47952,
    #[serde(rename = "48")]
    Fps48,
    #[serde(rename = "50")]
    Fps50,
    /// 59.94 (60000/1001), non-drop numbering.
    #[serde(rename = "59.94")]
    Fps5994,
    /// 59.94 drop-frame: four labels skipped every minute but every tenth.
    #[serde(rename = "59.94df")]
    Fps5994Drop,
    #[serde(rename = "60")]
    Fps60,
    #[serde(rename = "100")]
    Fps100,
    /// 119.88 (120000/1001).
    #[serde(rename = "119.88")]
    Fps11988,
    #[serde(rename = "120")]
    Fps120,
}

impl FrameRate {
    pub const ALL: [FrameRate; 15] = [
        Self::Fps23976,
        Self::Fps24,
        Self::Fps25,
        Self::Fps2997,
        Self::Fps2997Drop,
        Self::Fps30,
        Self::Fps47952,
        Self::Fps48,
        Self::Fps50,
        Self::Fps5994,
        Self::Fps5994Drop,
        Self::Fps60,
        Self::Fps100,
        Self::Fps11988,
        Self::Fps120,
    ];

    /// Frames per second as a fraction (numerator, denominator).
    pub fn ratio(self) -> (u32, u32) {
        match self {
            Self::Fps23976 => (24_000, 1001),
            Self::Fps24 => (24, 1),
            Self::Fps25 => (25, 1),
            Self::Fps2997 | Self::Fps2997Drop => (30_000, 1001),
            Self::Fps30 => (30, 1),
            Self::Fps47952 => (48_000, 1001),
            Self::Fps48 => (48, 1),
            Self::Fps50 => (50, 1),
            Self::Fps5994 | Self::Fps5994Drop => (60_000, 1001),
            Self::Fps60 => (60, 1),
            Self::Fps100 => (100, 1),
            Self::Fps11988 => (120_000, 1001),
            Self::Fps120 => (120, 1),
        }
    }

    /// Frames per second of real time.
    pub fn fps(self) -> f64 {
        let (n, d) = self.ratio();
        n as f64 / d as f64
    }

    /// Frame labels per second of timecode (30 for 29.97).
    pub fn nominal(self) -> u32 {
        let (n, d) = self.ratio();
        n.div_ceil(d)
    }

    /// Labels skipped each minute but every tenth (0: non-drop).
    pub fn dropped(self) -> u32 {
        match self {
            Self::Fps2997Drop => 2,
            Self::Fps5994Drop => 4,
            _ => 0,
        }
    }

    pub fn is_drop(self) -> bool {
        self.dropped() > 0
    }

    /// The stable name (files, preferences).
    pub fn id(self) -> &'static str {
        match self {
            Self::Fps23976 => "23.976",
            Self::Fps24 => "24",
            Self::Fps25 => "25",
            Self::Fps2997 => "29.97",
            Self::Fps2997Drop => "29.97df",
            Self::Fps30 => "30",
            Self::Fps47952 => "47.952",
            Self::Fps48 => "48",
            Self::Fps50 => "50",
            Self::Fps5994 => "59.94",
            Self::Fps5994Drop => "59.94df",
            Self::Fps60 => "60",
            Self::Fps100 => "100",
            Self::Fps11988 => "119.88",
            Self::Fps120 => "120",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.id() == id)
    }

    pub fn label(self) -> String {
        match self {
            Self::Fps2997Drop => "29.97 fps drop-frame".into(),
            Self::Fps5994Drop => "59.94 fps drop-frame".into(),
            r => format!("{} fps", r.id()),
        }
    }

    /// The SMPTE rate of a file's `num/den` frames per second (non-drop
    /// for the 1000/1001 rates; files rarely say), if one is within 0.1 %.
    pub fn from_fraction(num: u32, den: u32) -> Option<Self> {
        if num == 0 || den == 0 {
            return None;
        }
        let fps = num as f64 / den as f64;
        Self::ALL
            .into_iter()
            .filter(|r| !r.is_drop())
            .min_by(|a, b| (a.fps() - fps).abs().total_cmp(&(b.fps() - fps).abs()))
            .filter(|r| (r.fps() - fps).abs() <= r.fps() * 1e-3)
    }

    /// The same frames with the other numbering (29.97 ↔ 29.97 drop,
    /// 59.94 ↔ 59.94 drop); other rates have only one.
    pub fn with_drop(self, drop: bool) -> Self {
        match (self, drop) {
            (Self::Fps2997 | Self::Fps2997Drop, true) => Self::Fps2997Drop,
            (Self::Fps2997 | Self::Fps2997Drop, false) => Self::Fps2997,
            (Self::Fps5994 | Self::Fps5994Drop, true) => Self::Fps5994Drop,
            (Self::Fps5994 | Self::Fps5994Drop, false) => Self::Fps5994,
            (r, _) => r,
        }
    }

    /// The frame showing at `seconds` (frame 0 starts at 0).
    pub fn frame_at(self, seconds: f64) -> i64 {
        let (n, d) = self.ratio();
        // A hair forward: a frame's own start time lands in it.
        (seconds * n as f64 / d as f64 + 1e-7).floor() as i64
    }

    /// Where frame `frame` starts, in seconds.
    pub fn seconds_of(self, frame: i64) -> f64 {
        let (n, d) = self.ratio();
        frame as f64 * d as f64 / n as f64
    }
}

/// A timecode label (hours:minutes:seconds:frames).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Timecode {
    pub hours: u8,
    pub minutes: u8,
    pub seconds: u8,
    pub frames: u8,
}

impl Timecode {
    /// Frames since 00:00:00:00 at `rate` (drop-frame numbering skips its
    /// labels every minute but every tenth).
    pub fn total_frames(self, rate: FrameRate) -> i64 {
        let n = rate.nominal() as i64;
        let minutes = self.hours as i64 * 60 + self.minutes as i64;
        let labels = (minutes * 60 + self.seconds as i64) * n + self.frames as i64;
        labels - rate.dropped() as i64 * (minutes - minutes / 10)
    }

    /// The label of frame `frames` since 00:00:00:00 at `rate` (wrapping
    /// at 24 hours; negative frames count back from 24:00:00:00).
    pub fn from_frames(frames: i64, rate: FrameRate) -> Self {
        let n = rate.nominal() as i64;
        let drop = rate.dropped() as i64;
        let per_ten = 600 * n - 9 * drop;
        let day = 144 * per_ten;
        let mut f = frames.rem_euclid(day);
        if drop > 0 {
            let per_minute = 60 * n - drop;
            let tens = f / per_ten;
            let rest = f % per_ten;
            f += 9 * drop * tens
                + if rest >= drop {
                    drop * ((rest - drop) / per_minute)
                } else {
                    0
                };
        }
        Self {
            hours: (f / (n * 3600) % 24) as u8,
            minutes: (f / (n * 60) % 60) as u8,
            seconds: (f / n % 60) as u8,
            frames: (f % n) as u8,
        }
    }

    /// The label showing at `seconds` since 00:00:00:00.
    pub fn from_seconds(seconds: f64, rate: FrameRate) -> Self {
        Self::from_frames(rate.frame_at(seconds), rate)
    }

    pub fn to_seconds(self, rate: FrameRate) -> f64 {
        rate.seconds_of(self.total_frames(rate))
    }

    /// "hh:mm:ss:ff" ('.', ';' or ',' between also); `None` when a field
    /// is out of range for `rate` or names a label drop-frame skips.
    pub fn parse(s: &str, rate: FrameRate) -> Option<Self> {
        let parts: Vec<u8> = s
            .trim()
            .split([':', ';', '.', ','])
            .map(|p| p.trim().parse().ok())
            .collect::<Option<_>>()?;
        let [hours, minutes, seconds, frames] = parts[..] else {
            return None;
        };
        let tc = Self {
            hours,
            minutes,
            seconds,
            frames,
        };
        let skipped =
            rate.is_drop() && seconds == 0 && minutes % 10 != 0 && (frames as u32) < rate.dropped();
        (hours < 24 && minutes < 60 && seconds < 60 && (frames as u32) < rate.nominal() && !skipped)
            .then_some(tc)
    }

    /// "hh:mm:ss:ff", or "hh:mm:ss;ff" for drop-frame.
    pub fn display(self, rate: FrameRate) -> String {
        let sep = if rate.is_drop() { ';' } else { ':' };
        format!(
            "{:02}:{:02}:{:02}{sep}{:02}",
            self.hours, self.minutes, self.seconds, self.frames
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_and_frames_go_round_every_rate() {
        for rate in FrameRate::ALL {
            for f in (0..400_000).step_by(37).chain([0, 1, 1799, 1800, 17_982]) {
                let tc = Timecode::from_frames(f, rate);
                assert_eq!(tc.total_frames(rate), f, "{rate:?} frame {f}: {tc:?}");
                assert_eq!(Timecode::parse(&tc.display(rate), rate), Some(tc));
            }
        }
    }

    #[test]
    fn drop_frame_skips_its_labels() {
        let df = FrameRate::Fps2997Drop;
        assert_eq!(Timecode::from_frames(1799, df).display(df), "00:00:59;29");
        assert_eq!(Timecode::from_frames(1800, df).display(df), "00:01:00;02");
        assert_eq!(Timecode::from_frames(17_982, df).display(df), "00:10:00;00");
        // An hour of 29.97 drop is an hour of real time, near enough.
        let hour = Timecode::parse("01:00:00;00", df).unwrap();
        assert_eq!(hour.total_frames(df), 107_892);
        assert!((hour.to_seconds(df) - 3600.0).abs() < 0.004);
        assert_eq!(Timecode::parse("00:01:00;01", df), None, "a skipped label");
        let df60 = FrameRate::Fps5994Drop;
        assert_eq!(
            Timecode::from_frames(3600, df60).display(df60),
            "00:01:00;04"
        );
        assert_eq!(
            Timecode::from_frames(3599, df60).display(df60),
            "00:00:59;59"
        );
    }

    #[test]
    fn files_rates_match_the_smpte_rates() {
        assert_eq!(
            FrameRate::from_fraction(24_000, 1001),
            Some(FrameRate::Fps23976)
        );
        assert_eq!(
            FrameRate::from_fraction(30_000, 1001),
            Some(FrameRate::Fps2997)
        );
        assert_eq!(FrameRate::from_fraction(25, 1), Some(FrameRate::Fps25));
        assert_eq!(
            FrameRate::from_fraction(2997, 100),
            Some(FrameRate::Fps2997)
        );
        assert_eq!(FrameRate::from_fraction(15, 1), None);
        assert_eq!(FrameRate::Fps2997.nominal(), 30);
        for r in FrameRate::ALL {
            assert_eq!(FrameRate::from_id(r.id()), Some(r));
        }
    }

    #[test]
    fn a_frame_starts_where_it_says() {
        for rate in FrameRate::ALL {
            for f in [0, 1, 2, 1000, 86_399] {
                assert_eq!(rate.frame_at(rate.seconds_of(f)), f, "{rate:?}");
            }
        }
    }
}
