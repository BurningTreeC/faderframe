use faderframe_core::gain_to_db;
use faderframe_engine::TrackMeter;

/// Lowest level drawn by meters.
pub const METER_FLOOR_DB: f32 = -72.0;
/// Fall-back rate of the bar (dB per second), roughly IEC "fast" PPM-like.
pub const FALLBACK_DB_PER_S: f32 = 26.0;
/// How long a peak-hold marker stays before falling.
pub const PEAK_HOLD_S: f32 = 1.6;

/// The RMS meters' integration (seconds, on the power).
pub const RMS_TIME_S: f32 = 0.3;
/// A VU meter's movement: second order, 99 % of a step in 300 ms with
/// 1.5 % overshoot (IEC 60268-17).
const VU_OMEGA: f32 = 13.1;
const VU_ZETA: f32 = 0.8;
/// The PPM's fall when no audio comes (dB per second; EBU).
const PPM_FALL_DB_PER_S: f32 = faderframe_realtime::PPM_FALL_DB_PER_S;

/// Display state of one meter channel with ballistics applied on the UI
/// side: sample peaks with a hold, the RMS level, a VU needle and the
/// quasi-peak reading (the engine reports raw peaks, energy and the PPM's
/// envelope).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeterChannel {
    /// Displayed bar level in dBFS (sample peaks).
    pub level_db: f32,
    /// RMS level in dBFS ([`RMS_TIME_S`] on the power).
    pub rms_db: f32,
    power: f32,
    pub hold_db: f32,
    hold_age: f32,
    /// Latched when a sample reached or exceeded 0 dBFS.
    pub clipped: bool,
    /// The VU needle: the RMS voltage (linear, full scale 1) as the
    /// movement shows it; a view scales it by its reference.
    pub vu: f32,
    vu_speed: f32,
    /// The quasi-peak programme level in dBFS (when PPM metering is on).
    pub ppm_db: f32,
}

impl Default for MeterChannel {
    fn default() -> Self {
        Self {
            level_db: METER_FLOOR_DB,
            rms_db: METER_FLOOR_DB,
            power: 0.0,
            hold_db: METER_FLOOR_DB,
            hold_age: 0.0,
            clipped: false,
            vu: 0.0,
            vu_speed: 0.0,
            ppm_db: METER_FLOOR_DB,
        }
    }
}

impl MeterChannel {
    /// Integrate a new raw reading over `dt` seconds.
    pub fn update(&mut self, r: &faderframe_realtime::MeterReading, dt: f32) {
        let peak = r.peak;
        let peak_db = gain_to_db(peak).max(METER_FLOOR_DB);
        let fallen = self.level_db - FALLBACK_DB_PER_S * dt;
        self.level_db = peak_db.max(fallen).max(METER_FLOOR_DB);
        // The RMS of what came (none: silence while nothing plays).
        let mean = if r.frames > 0 { r.mean } else { 0.0 };
        let a = 1.0 - (-dt / RMS_TIME_S).exp();
        self.power += (mean * mean - self.power) * a;
        self.rms_db = gain_to_db(self.power.max(0.0).sqrt()).max(METER_FLOOR_DB);
        // The needle, in 1 ms steps.
        let steps = ((dt * 1000.0).ceil() as usize).clamp(1, 1000);
        let h = dt / steps as f32;
        for _ in 0..steps {
            let accel =
                VU_OMEGA * VU_OMEGA * (mean - self.vu) - 2.0 * VU_ZETA * VU_OMEGA * self.vu_speed;
            self.vu_speed += accel * h;
            self.vu += self.vu_speed * h;
        }
        // Against its stops.
        if self.vu < 0.0 {
            self.vu = 0.0;
            self.vu_speed = self.vu_speed.max(0.0);
        } else if self.vu > 2.0 {
            self.vu = 2.0;
            self.vu_speed = self.vu_speed.min(0.0);
        }
        self.ppm_db = if r.frames > 0 {
            gain_to_db(r.ppm).max(METER_FLOOR_DB)
        } else {
            (self.ppm_db - PPM_FALL_DB_PER_S * dt).max(METER_FLOOR_DB)
        };
        if peak_db >= self.hold_db {
            self.hold_db = peak_db;
            self.hold_age = 0.0;
        } else {
            self.hold_age += dt;
            if self.hold_age > PEAK_HOLD_S {
                self.hold_db = (self.hold_db - FALLBACK_DB_PER_S * 2.0 * dt).max(self.level_db);
            }
        }
        if peak >= 1.0 {
            self.clipped = true;
        }
    }

    /// The VU reading for a reference level (0 VU at `reference` dBFS RMS).
    pub fn vu_db(&self, reference: f32) -> f32 {
        gain_to_db(self.vu.max(1e-9)) - reference
    }

    /// Is anything still moving (needs redraws)?
    pub fn is_active(&self) -> bool {
        self.level_db > METER_FLOOR_DB + 0.01
            || self.hold_db > METER_FLOOR_DB + 0.01
            || self.rms_db > METER_FLOOR_DB + 0.01
            || self.ppm_db > METER_FLOOR_DB + 0.01
            || self.vu > 1e-5
            || self.vu_speed.abs() > 1e-5
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MeterDisplay {
    pub left: MeterChannel,
    pub right: MeterChannel,
    /// Every channel (a surround bed's; `count` of them).
    pub channels: [MeterChannel; faderframe_engine::METER_MAX],
    pub count: usize,
}

impl MeterDisplay {
    pub fn update(&mut self, m: &TrackMeter, dt: f32) {
        self.left.update(&m.left, dt);
        self.right.update(&m.right, dt);
        self.count = m.count;
        for (d, r) in self.channels.iter_mut().zip(&m.channels).take(m.count) {
            d.update(r, dt);
        }
    }

    /// The channels to draw: every one of a bed, else left and right.
    pub fn shown(&self) -> &[MeterChannel] {
        &self.channels[..self.count.max(2).min(self.channels.len())]
    }

    pub fn is_active(&self) -> bool {
        self.left.is_active()
            || self.right.is_active()
            || self.channels[..self.count]
                .iter()
                .any(MeterChannel::is_active)
    }

    pub fn reset_clip(&mut self) {
        self.left.clipped = false;
        self.right.clipped = false;
        for c in &mut self.channels {
            c.clipped = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use faderframe_realtime::MeterReading;

    fn reading(peak: f32, mean: f32) -> MeterReading {
        MeterReading {
            peak,
            rms: mean,
            mean,
            frames: 800,
            ppm: peak,
        }
    }

    #[test]
    fn ballistics_rise_instantly_and_fall_slowly() {
        let mut m = MeterChannel::default();
        m.update(&reading(0.5, 0.3), 1.0 / 60.0);
        let l = m.level_db;
        assert!((l - (-6.02)).abs() < 0.05);
        m.update(&reading(0.0, 0.0), 0.1);
        assert!((m.level_db - (l - 2.6)).abs() < 0.01);
        assert!((m.hold_db - l).abs() < 1e-3, "hold stays");
        for _ in 0..200 {
            m.update(&reading(0.0, 0.0), 0.05);
        }
        assert_eq!(m.level_db, METER_FLOOR_DB);
        assert!(!m.is_active());
        m.update(&reading(1.2, 0.5), 0.01);
        assert!(m.clipped);
    }

    /// A tone at the reference: 0 VU within 300 ms (99 %), at most 1.5 %
    /// over on the way; the RMS settles at its level.
    #[test]
    fn the_vu_needle_moves_like_a_vu_meter() {
        let reference = -18.0f32;
        let rms = 10f32.powf(reference / 20.0);
        let mut m = MeterChannel::default();
        let mut most = 0.0f32;
        let mut at_300 = 0.0;
        for i in 1..=120 {
            m.update(&reading(rms * 1.414, rms), 1.0 / 60.0);
            most = most.max(m.vu);
            if i == 18 {
                at_300 = m.vu / rms;
            }
        }
        assert!(at_300 > 0.98, "after 300 ms: {at_300:.3}");
        assert!(most / rms < 1.02, "overshoot: {:.3}", most / rms);
        assert!(m.vu_db(reference).abs() < 0.05, "{} VU", m.vu_db(reference));
        assert!((m.rms_db - reference).abs() < 0.1, "{}", m.rms_db);
    }
}
