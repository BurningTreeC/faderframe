//! Frequency-dependent width and fold-down per band of a profile (its
//! long-term powers, every frame weighing the same; see [`crate::stereo`]
//! for mid and side):
//!
//! * **width** = side power over mid power (dB): very negative for a
//!   centred band, 0 dB for uncorrelated channels of equal level or a
//!   source panned to one side, positive as the channels approach inverted
//!   copies;
//! * **mono change** = mid power over the channels' mean power (dB): 0 for
//!   a centred band, −3 dB for one side alone or uncorrelated channels,
//!   falling without bound towards inverted ones. Unlike
//!   [`crate::stereo::BandStereo::fold_db`] (against the sum without
//!   cancellation) this includes what panning costs a mono speaker;
//! * the **shift** of a band = its mono change minus the mono change of
//!   all relevant bands together: how the balance moves in mono (negative:
//!   that band gets quieter against the rest).
//!
//! Low correlation is not judged here: these are measurements.

use crate::bands::BandLayout;
use crate::details::WidthSummary;
use crate::finding::Subject;
use crate::profile::{FLOOR_DB, TrackProfile, power_db};
use crate::stereo;

/// Widths are shown within ±60 dB.
const LIMIT_DB: f64 = 60.0;

/// One band's long-term powers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BandWidth {
    pub centre: f64,
    pub lo: f64,
    pub hi: f64,
    pub mid: f64,
    pub side: f64,
    /// The channels' mean power.
    pub power: f64,
}

impl BandWidth {
    pub fn level_db(&self) -> f64 {
        power_db(self.power)
    }

    /// Side over mid (dB; `None` in silence).
    pub fn width_db(&self) -> Option<f64> {
        if self.power <= 0.0 || self.level_db() <= FLOOR_DB {
            return None;
        }
        Some((power_db(self.side) - power_db(self.mid)).clamp(-LIMIT_DB, LIMIT_DB))
    }

    /// Mid over the channels' mean (dB; `None` in silence).
    pub fn mono_db(&self) -> Option<f64> {
        if self.power <= 0.0 || self.level_db() <= FLOOR_DB {
            return None;
        }
        Some((power_db(self.mid) - power_db(self.power)).max(-LIMIT_DB))
    }
}

/// Every band of `p` over all its frames.
pub fn bands(p: &TrackProfile) -> Vec<BandWidth> {
    stereo::bands(p, None)
        .iter()
        .zip(p.layout.bands())
        .map(|(s, b)| BandWidth {
            centre: b.centre,
            lo: b.lo,
            hi: b.hi,
            mid: s.mid,
            side: s.side,
            power: s.power,
        })
        .collect()
}

/// Bands within `depth` dB of the loudest and above `floor` dB.
pub fn relevant(bands: &[BandWidth], depth: f64, floor: f64) -> Vec<bool> {
    let top = bands
        .iter()
        .map(BandWidth::level_db)
        .fold(f64::NEG_INFINITY, f64::max);
    bands
        .iter()
        .map(|b| b.level_db() >= (top - depth).max(floor))
        .collect()
}

/// The mono change of the `relevant` bands together (dB).
pub fn whole_mono_db(bands: &[BandWidth], relevant: &[bool]) -> Option<f64> {
    let (mut mid, mut power) = (0.0, 0.0);
    for (b, r) in bands.iter().zip(relevant) {
        if *r {
            mid += b.mid;
            power += b.power;
        }
    }
    (power > 0.0).then(|| power_db(mid) - power_db(power))
}

/// The summary of a stereo subject (`None` for mono profiles).
pub fn summary(subject: Subject, p: &TrackProfile) -> Option<WidthSummary> {
    if p.channels < 2 {
        return None;
    }
    let b = bands(p);
    let nan = |v: Option<f64>| v.unwrap_or(f64::NAN);
    let broad = BandLayout::broad();
    let region = |r: &crate::bands::Band| -> (f64, f64) {
        let (mut mid, mut side, mut power) = (0.0, 0.0, 0.0);
        for x in b.iter().filter(|x| x.centre >= r.lo && x.centre < r.hi) {
            mid += x.mid;
            side += x.side;
            power += x.power;
        }
        let w = BandWidth {
            centre: 0.0,
            lo: r.lo,
            hi: r.hi,
            mid,
            side,
            power,
        };
        (nan(w.width_db()), nan(w.mono_db()))
    };
    let (broad_width_db, broad_mono_db) = broad.bands().iter().map(region).unzip();
    Some(WidthSummary {
        subject,
        centres: b.iter().map(|x| x.centre).collect(),
        width_db: b.iter().map(|x| nan(x.width_db())).collect(),
        mono_db: b.iter().map(|x| nan(x.mono_db())).collect(),
        broad_width_db,
        broad_mono_db,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phase::tests::Noise;
    use crate::test_support::{SR, profile, tone};

    fn noise(seed: u64, amp: f64) -> Vec<f32> {
        let mut n = Noise(seed);
        (0..SR as usize * 4)
            .map(|_| (amp * n.next()) as f32)
            .collect()
    }

    #[test]
    fn widths_and_mono_changes_of_known_images() {
        let at = |p: &TrackProfile, hz: f64| {
            let b = bands(p);
            let i = p.layout.index_of(hz).unwrap_or(0);
            (
                b[i].width_db().unwrap_or(f64::NAN),
                b[i].mono_db().unwrap_or(f64::NAN),
            )
        };
        // Centred: no side, nothing lost in mono.
        let s = tone(1000.0, 0.3, 0.0, 4.0, 4.0);
        let (w, m) = at(&profile(&[&s, &s]), 1000.0);
        assert!(w <= -59.0 && m.abs() < 0.01, "{w} {m}");
        // Uncorrelated noise left and right: as wide as it is centred,
        // 3 dB down in mono.
        let (l, r) = (noise(3, 0.3), noise(4, 0.3));
        let (w, m) = at(&profile(&[&l, &r]), 2000.0);
        assert!(w.abs() < 0.5 && (m + 3.01).abs() < 0.3, "{w} {m}");
        // Panned hard left: also 0 dB wide, 3 dB down in mono.
        let z = vec![0.0f32; s.len()];
        let (w, m) = at(&profile(&[&s, &z]), 1000.0);
        assert!(w.abs() < 0.01 && (m + 3.01).abs() < 0.01, "{w} {m}");
        // Inverted: side only, gone in mono.
        let inv: Vec<f32> = s.iter().map(|v| -v).collect();
        let (w, m) = at(&profile(&[&s, &inv]), 1000.0);
        assert!(w >= 59.0 && m <= -59.0, "{w} {m}");
        // A mono profile has no width summary; a stereo one per band and
        // region.
        assert!(summary(Subject::Track(faderframe_core::TrackId(1)), &profile(&[&s])).is_none());
        let sm = summary(
            Subject::Track(faderframe_core::TrackId(1)),
            &profile(&[&l, &r]),
        );
        let sm = sm.unwrap_or_else(|| panic!("a summary"));
        assert_eq!(sm.broad_width_db.len(), crate::bands::BROAD_NAMES.len());
        assert!(sm.broad_width_db[4].abs() < 0.5, "{:?}", sm.broad_width_db);
        // Silence: nothing.
        let (w, m) = at(&profile(&[&z, &z]), 1000.0);
        assert!(w.is_nan() && m.is_nan());
    }
}
