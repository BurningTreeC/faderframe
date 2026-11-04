//! Streaming platforms as a master meets them: the loudness they play it
//! at (turning loud masters down, some turning quiet ones up as far as
//! their peaks allow) and the lossy codec of their usual tier.
//!
//! The figures are the platforms' published or widely measured defaults
//! (October 2026); services change them, and several offer lossless tiers
//! besides. Where a platform's codec (Ogg Vorbis, AAC, MP3) has no encoder
//! under a permissive licence, Opus at the same bitrate stands in for it
//! ([`Platform::native`] false): its artefacts differ in kind, their
//! level and the true peak after decoding compare.

/// What a platform does with a master quieter than its target.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TurnUp {
    /// Left as it is (only loud masters are turned down).
    Never,
    /// Turned up, but only until its true peak reaches the ceiling.
    ToPeak { ceiling_dbtp: f64 },
}

/// A streaming platform's playback of a master.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Platform {
    /// Stable id (`spotify`, `apple-music`, …).
    pub id: &'static str,
    pub name: &'static str,
    /// The integrated loudness it plays masters at (LUFS).
    pub target_lufs: f64,
    pub turn_up: TurnUp,
    /// Its usual tier's codec and bitrate (kb/s, stereo).
    pub codec: &'static str,
    pub kbps: u32,
    pub vbr: bool,
    /// The codec is Opus (else Opus at the same bitrate stands in).
    pub native: bool,
    /// Its normalisation and tiers, in a sentence.
    pub note: &'static str,
}

/// The platforms, in the order menus list them.
pub const PLATFORMS: [Platform; 6] = [
    Platform {
        id: "spotify",
        name: "Spotify",
        target_lufs: -14.0,
        turn_up: TurnUp::ToPeak { ceiling_dbtp: -1.0 },
        codec: "Ogg Vorbis",
        kbps: 320,
        vbr: true,
        native: false,
        note: "Normal volume (the default; Loud is −11 LUFS with a limiter, Quiet −19): \
               quiet masters are turned up only until their peaks reach −1 dBTP; Very High \
               quality streams Ogg Vorbis at 320 kb/s, Lossless plays FLAC",
    },
    Platform {
        id: "apple-music",
        name: "Apple Music",
        target_lufs: -16.0,
        turn_up: TurnUp::ToPeak { ceiling_dbtp: -1.0 },
        codec: "AAC",
        kbps: 256,
        vbr: true,
        native: false,
        note: "Sound Check plays masters at −16 LUFS, turning quiet ones up only as far as \
               their peaks allow; streams AAC at 256 kb/s, Lossless plays ALAC",
    },
    Platform {
        id: "youtube",
        name: "YouTube",
        target_lufs: -14.0,
        turn_up: TurnUp::Never,
        codec: "Opus",
        kbps: 160,
        vbr: true,
        native: true,
        note: "Loud uploads are turned down to −14 LUFS, quiet ones left as they are; music \
               streams as Opus at about 160 kb/s",
    },
    Platform {
        id: "tidal",
        name: "Tidal",
        target_lufs: -14.0,
        turn_up: TurnUp::Never,
        codec: "AAC",
        kbps: 320,
        vbr: true,
        native: false,
        note: "Normalised to −14 LUFS (by album when an album plays), never turned up; High \
               streams AAC at 320 kb/s, Max plays FLAC",
    },
    Platform {
        id: "amazon-music",
        name: "Amazon Music",
        target_lufs: -14.0,
        turn_up: TurnUp::Never,
        codec: "AAC",
        kbps: 256,
        vbr: true,
        native: false,
        note: "Loud masters turned down to about −14 LUFS; standard quality streams lossy at \
               about 256 kb/s, HD and Ultra HD play FLAC",
    },
    Platform {
        id: "deezer",
        name: "Deezer",
        target_lufs: -15.0,
        turn_up: TurnUp::Never,
        codec: "MP3",
        kbps: 320,
        vbr: false,
        native: false,
        note: "Normalised to −15 LUFS, never turned up; High Quality streams MP3 at 320 kb/s \
               (constant), HiFi plays FLAC",
    },
];

/// A platform by its id.
pub fn platform(id: &str) -> Option<&'static Platform> {
    PLATFORMS.iter().find(|p| p.id.eq_ignore_ascii_case(id))
}

/// The gain a platform plays a master at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlatformGain {
    /// What it applies (dB).
    pub db: f64,
    /// What would bring the master to the target (dB).
    pub wanted_db: f64,
    /// Turning up stopped short of the target (the peaks).
    pub peak_limited: bool,
}

impl Platform {
    /// The gain for a master of `integrated` LUFS whose true peak is
    /// `true_peak` dBTP (0 dB for silence).
    pub fn gain(&self, integrated: f64, true_peak: f64) -> PlatformGain {
        if !integrated.is_finite() {
            return PlatformGain {
                db: 0.0,
                wanted_db: 0.0,
                peak_limited: false,
            };
        }
        let wanted = self.target_lufs - integrated;
        if wanted <= 0.0 {
            return PlatformGain {
                db: wanted,
                wanted_db: wanted,
                peak_limited: false,
            };
        }
        match self.turn_up {
            TurnUp::Never => PlatformGain {
                db: 0.0,
                wanted_db: wanted,
                peak_limited: false,
            },
            TurnUp::ToPeak { ceiling_dbtp } => {
                let room = if true_peak.is_finite() {
                    (ceiling_dbtp - true_peak).max(0.0)
                } else {
                    wanted
                };
                PlatformGain {
                    db: wanted.min(room),
                    wanted_db: wanted,
                    peak_limited: room < wanted,
                }
            }
        }
    }

    /// "Ogg Vorbis 320 kb/s (Opus 320 kb/s stands in)".
    pub fn codec_text(&self) -> String {
        let mode = if self.vbr { "" } else { " CBR" };
        if self.native {
            format!("{} {} kb/s{mode}", self.codec, self.kbps)
        } else {
            format!(
                "{} {} kb/s{mode} (emulated: Opus {} kb/s)",
                self.codec, self.kbps, self.kbps
            )
        }
    }

    /// What it does with quiet masters, in words.
    pub fn turn_up_text(&self) -> String {
        match self.turn_up {
            TurnUp::Never => "never turns quiet masters up".into(),
            TurnUp::ToPeak { ceiling_dbtp } => format!(
                "turns quiet masters up until their peaks reach {:.0} dBTP",
                ceiling_dbtp
            )
            .replace('-', "−"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loud_masters_are_turned_down_quiet_ones_as_the_platform_does() {
        let spotify = platform("spotify").unwrap();
        let g = spotify.gain(-8.0, -0.2);
        assert_eq!(g.db, -6.0);
        assert!(!g.peak_limited);
        // Turned up until the peaks reach −1 dBTP.
        let g = spotify.gain(-20.0, -4.0);
        assert_eq!(g.wanted_db, 6.0);
        assert_eq!(g.db, 3.0);
        assert!(g.peak_limited);
        let g = spotify.gain(-17.0, -9.0);
        assert_eq!(g.db, 3.0);
        assert!(!g.peak_limited);
        // Never turned up.
        let tidal = platform("TIDAL").unwrap();
        assert_eq!(tidal.gain(-20.0, -10.0).db, 0.0);
        assert_eq!(tidal.gain(-10.0, -1.0).db, -4.0);
        assert_eq!(platform("apple-music").unwrap().gain(-12.0, -1.0).db, -4.0);
        // Silence is left alone.
        assert_eq!(spotify.gain(f64::NEG_INFINITY, f64::NEG_INFINITY).db, 0.0);
        assert!(platform("napster").is_none());
    }

    #[test]
    fn ids_are_unique_and_stand_ins_say_so() {
        for (i, p) in PLATFORMS.iter().enumerate() {
            assert!(PLATFORMS[..i].iter().all(|q| q.id != p.id));
            assert_eq!(p.codec_text().contains("emulated"), !p.native, "{}", p.id);
        }
    }
}
