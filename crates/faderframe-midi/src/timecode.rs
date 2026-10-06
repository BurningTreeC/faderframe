//! SMPTE timecode as MIDI time code carries it: the four MTC frame rates
//! (drop-frame numbering for 29.97) and hours:minutes:seconds:frames.

/// MTC frame rates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MtcRate {
    Fps24,
    #[default]
    Fps25,
    /// 29.97 fps drop-frame.
    Fps2997Drop,
    Fps30,
}

impl MtcRate {
    pub const ALL: [MtcRate; 4] = [
        MtcRate::Fps24,
        MtcRate::Fps25,
        MtcRate::Fps2997Drop,
        MtcRate::Fps30,
    ];

    /// A stable name (preferences).
    pub fn id(self) -> &'static str {
        match self {
            Self::Fps24 => "24",
            Self::Fps25 => "25",
            Self::Fps2997Drop => "29.97df",
            Self::Fps30 => "30",
        }
    }

    pub fn from_id(id: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|r| r.id() == id)
            .unwrap_or_default()
    }

    /// The rate of MTC's two rate bits.
    pub fn from_bits(bits: u8) -> Self {
        match bits & 3 {
            0 => Self::Fps24,
            1 => Self::Fps25,
            2 => Self::Fps2997Drop,
            _ => Self::Fps30,
        }
    }

    /// Frames per second of real time.
    pub fn fps(self) -> f64 {
        match self {
            Self::Fps24 => 24.0,
            Self::Fps25 => 25.0,
            Self::Fps2997Drop => 30_000.0 / 1001.0,
            Self::Fps30 => 30.0,
        }
    }

    /// Frame labels per second (30 for drop-frame).
    pub fn nominal(self) -> u32 {
        match self {
            Self::Fps24 => 24,
            Self::Fps25 => 25,
            Self::Fps2997Drop | Self::Fps30 => 30,
        }
    }

    /// MTC's two rate bits.
    pub fn bits(self) -> u8 {
        match self {
            Self::Fps24 => 0,
            Self::Fps25 => 1,
            Self::Fps2997Drop => 2,
            Self::Fps30 => 3,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Fps24 => "24 fps",
            Self::Fps25 => "25 fps",
            Self::Fps2997Drop => "29.97 fps drop",
            Self::Fps30 => "30 fps",
        }
    }
}

/// A timecode (hours:minutes:seconds:frames).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Timecode {
    pub hours: u8,
    pub minutes: u8,
    pub seconds: u8,
    pub frames: u8,
}

impl Timecode {
    /// Frames since 00:00:00:00 at `rate` (drop-frame numbering skips two
    /// frame labels every minute except every tenth).
    pub fn total_frames(self, rate: MtcRate) -> i64 {
        let n = rate.nominal() as i64;
        let minutes = self.hours as i64 * 60 + self.minutes as i64;
        let labels = (minutes * 60 + self.seconds as i64) * n + self.frames as i64;
        match rate {
            MtcRate::Fps2997Drop => labels - 2 * (minutes - minutes / 10),
            _ => labels,
        }
    }

    pub fn to_seconds(self, rate: MtcRate) -> f64 {
        self.total_frames(rate) as f64 / rate.fps()
    }

    /// The timecode of a moment (`seconds` ≥ 0) at `rate`.
    pub fn from_seconds(seconds: f64, rate: MtcRate) -> Self {
        Self::from_frames((seconds.max(0.0) * rate.fps() + 1e-6).floor() as i64, rate)
    }

    /// The timecode of frame `frames` (≥ 0) since 00:00:00:00 at `rate`.
    pub fn from_frames(frames: i64, rate: MtcRate) -> Self {
        let mut frames = frames.max(0);
        let n = rate.nominal() as i64;
        if rate == MtcRate::Fps2997Drop {
            // Add the skipped labels back (17982 frames per 10 minutes).
            let tens = frames / 17_982;
            let rest = frames % 17_982;
            frames += 18 * tens + if rest > 1 { 2 * ((rest - 2) / 1798) } else { 0 };
        }
        Self {
            hours: (frames / (n * 3600) % 24) as u8,
            minutes: (frames / (n * 60) % 60) as u8,
            seconds: (frames / n % 60) as u8,
            frames: (frames % n) as u8,
        }
    }

    /// "hh:mm:ss:ff" (also accepts '.' or ';' as separators).
    pub fn parse(s: &str) -> Option<Self> {
        let parts: Vec<u8> = s
            .trim()
            .split([':', ';', '.'])
            .map(|p| p.trim().parse().ok())
            .collect::<Option<_>>()?;
        let [hours, minutes, seconds, frames] = parts[..] else {
            return None;
        };
        (hours < 24 && minutes < 60 && seconds < 60 && frames < 30).then_some(Self {
            hours,
            minutes,
            seconds,
            frames,
        })
    }
}

impl std::fmt::Display for Timecode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:02}:{:02}:{:02}:{:02}",
            self.hours, self.minutes, self.seconds, self.frames
        )
    }
}
