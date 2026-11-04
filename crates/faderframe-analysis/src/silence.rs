//! Finding the sound between silences (Strip Silence, batch trimming).
//!
//! A signal is measured once into [`Levels`] — the peak over all channels
//! of every [`HOP`] frames — and [`regions`] then finds the spans with
//! sound for any [`SilenceSettings`] at once, so a dialog can preview
//! every turn of a knob: blocks at or above the threshold are sound, gaps
//! shorter than the minimum gap are bridged, spans shorter than the
//! minimum length go, and what is left is padded before and after (spans
//! the padding joins become one).

/// Frames per level (1.3 ms at 48 kHz).
pub const HOP: usize = 64;

/// How silence is told from sound.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SilenceSettings {
    /// Sound is at or above this peak level (dBFS).
    pub threshold_db: f32,
    /// Sound shorter than this is dropped (seconds).
    pub min_length: f64,
    /// Kept before each region's first sound (seconds).
    pub pre_pad: f64,
    /// Kept after each region's last sound (seconds).
    pub post_pad: f64,
    /// Silences shorter than this are kept inside a region (seconds).
    pub min_gap: f64,
}

impl Default for SilenceSettings {
    fn default() -> Self {
        Self {
            threshold_db: -48.0,
            min_length: 0.05,
            pre_pad: 0.005,
            post_pad: 0.05,
            min_gap: 0.1,
        }
    }
}

impl SilenceSettings {
    /// Every value in its range (NaNs to the defaults).
    pub fn clamped(self) -> Self {
        let d = Self::default();
        let s = |v: f64, d: f64| if v.is_finite() { v.clamp(0.0, 60.0) } else { d };
        Self {
            threshold_db: if self.threshold_db.is_finite() {
                self.threshold_db.clamp(-120.0, 0.0)
            } else {
                d.threshold_db
            },
            min_length: s(self.min_length, d.min_length),
            pre_pad: s(self.pre_pad, d.pre_pad),
            post_pad: s(self.post_pad, d.post_pad),
            min_gap: s(self.min_gap, d.min_gap),
        }
    }
}

/// Peak levels of a signal, one per [`HOP`] frames.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Levels {
    /// Linear peaks over all channels.
    pub peaks: Vec<f32>,
    /// Frames measured.
    pub frames: i64,
}

impl Levels {
    /// Measure planar channels (the shortest decides the length).
    pub fn measure(channels: &[&[f32]]) -> Self {
        let frames = channels.iter().map(|c| c.len()).min().unwrap_or(0);
        let mut levels = Self::default();
        levels.extend(channels, frames);
        levels
    }

    /// Measure `frames` more frames of planar channels (a streamed read:
    /// pass whole hops but for the last piece).
    pub fn extend(&mut self, channels: &[&[f32]], frames: usize) {
        let mut at = 0;
        while at < frames {
            let n = HOP.min(frames - at);
            let peak = channels
                .iter()
                .flat_map(|c| c.get(at..at + n).unwrap_or(&[]).iter())
                .fold(0.0f32, |m, v| m.max(v.abs()));
            self.peaks.push(peak);
            at += n;
        }
        self.frames += frames as i64;
    }
}

/// A span of sound, in frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: i64,
    pub end: i64,
}

impl Span {
    pub fn len(&self) -> i64 {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
}

/// The spans with sound in `levels` (a signal at `rate`).
pub fn regions(levels: &Levels, rate: f64, settings: &SilenceSettings) -> Vec<Span> {
    let s = settings.clamped();
    let threshold = 10f32.powf(s.threshold_db / 20.0);
    let frames = levels.frames;
    let secs = |v: f64| (v * rate).round() as i64;
    // Runs of blocks with sound.
    let mut spans: Vec<Span> = Vec::new();
    let mut open: Option<i64> = None;
    for (i, &p) in levels.peaks.iter().enumerate() {
        let at = (i * HOP) as i64;
        match (p >= threshold, open) {
            (true, None) => open = Some(at),
            (false, Some(a)) => {
                spans.push(Span { start: a, end: at });
                open = None;
            }
            _ => {}
        }
    }
    if let Some(a) = open {
        spans.push(Span {
            start: a,
            end: frames,
        });
    }
    // Short silences kept inside.
    let gap = secs(s.min_gap);
    let mut joined: Vec<Span> = Vec::with_capacity(spans.len());
    for sp in spans {
        match joined.last_mut() {
            Some(last) if sp.start - last.end < gap => last.end = sp.end,
            _ => joined.push(sp),
        }
    }
    // Short sounds dropped, then padded.
    let min = secs(s.min_length);
    let (pre, post) = (secs(s.pre_pad), secs(s.post_pad));
    let mut out: Vec<Span> = Vec::with_capacity(joined.len());
    for sp in joined.into_iter().filter(|sp| sp.len() >= min.max(1)) {
        let sp = Span {
            start: (sp.start - pre).max(0),
            end: (sp.end + post).min(frames),
        };
        match out.last_mut() {
            Some(last) if sp.start <= last.end => last.end = last.end.max(sp.end),
            _ => out.push(sp),
        }
    }
    out
}

/// The silences between and around `spans` within `[0, frames)`.
pub fn silences(spans: &[Span], frames: i64) -> Vec<Span> {
    let mut out = Vec::new();
    let mut at = 0;
    for sp in spans {
        if sp.start > at {
            out.push(Span {
                start: at,
                end: sp.start,
            });
        }
        at = at.max(sp.end);
    }
    if at < frames {
        out.push(Span {
            start: at,
            end: frames,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three hits of 50 ms at 0.5, 1.0 and 1.03 s in 2 s of silence.
    fn hits(rate: usize) -> Vec<f32> {
        let mut v = vec![0.0f32; 2 * rate];
        for at in [0.5, 1.0, 1.03] {
            let a = (at * rate as f64) as usize;
            for (k, s) in v[a..a + rate / 20].iter_mut().enumerate() {
                *s = 0.5 * (k as f32 * 0.3).sin();
            }
        }
        v
    }

    #[test]
    fn hits_are_found_padded_and_joined_across_short_gaps() {
        let rate = 48_000;
        let sig = hits(rate);
        let levels = Levels::measure(&[&sig]);
        assert_eq!(levels.frames, sig.len() as i64);
        let settings = SilenceSettings {
            threshold_db: -40.0,
            min_length: 0.01,
            pre_pad: 0.0,
            post_pad: 0.0,
            min_gap: 0.1,
        };
        let r = regions(&levels, rate as f64, &settings);
        // The two hits 30 ms apart (overlapping) are one region.
        assert_eq!(r.len(), 2, "{r:?}");
        let near = |a: i64, b: f64| (a - (b * rate as f64) as i64).abs() <= HOP as i64;
        assert!(near(r[0].start, 0.5) && near(r[0].end, 0.55), "{r:?}");
        assert!(near(r[1].start, 1.0) && near(r[1].end, 1.08), "{r:?}");
        // Padding widens them; a long minimum length drops them all.
        let padded = regions(
            &levels,
            rate as f64,
            &SilenceSettings {
                pre_pad: 0.01,
                post_pad: 0.1,
                ..settings
            },
        );
        assert!(near(padded[0].start, 0.49) && near(padded[0].end, 0.65));
        let none = regions(
            &levels,
            rate as f64,
            &SilenceSettings {
                min_length: 0.5,
                ..settings
            },
        );
        assert!(none.is_empty());
        let quiet = silences(&r, levels.frames);
        assert_eq!(quiet.len(), 3);
        assert_eq!(quiet[0].start, 0);
        assert_eq!(quiet[2].end, levels.frames);
    }

    #[test]
    fn levels_measured_in_pieces_are_the_whole_ones() {
        let sig = hits(8_000);
        let whole = Levels::measure(&[&sig]);
        let mut parts = Levels::default();
        for c in sig.chunks(HOP * 10) {
            parts.extend(&[c], c.len());
        }
        assert_eq!(whole, parts);
    }
}
