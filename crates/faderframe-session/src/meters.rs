use faderframe_core::gain_to_db;
use faderframe_engine::TrackMeter;

/// Lowest level drawn by meters.
pub const METER_FLOOR_DB: f32 = -72.0;
/// Fall-back rate of the bar (dB per second), roughly IEC "fast" PPM-like.
pub const FALLBACK_DB_PER_S: f32 = 26.0;
/// How long a peak-hold marker stays before falling.
pub const PEAK_HOLD_S: f32 = 1.6;

/// Display state of one meter channel with ballistics applied on the UI
/// side (the engine only reports raw peaks).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeterChannel {
    /// Displayed bar level in dBFS.
    pub level_db: f32,
    /// RMS level in dBFS (slower, informational).
    pub rms_db: f32,
    pub hold_db: f32,
    hold_age: f32,
    /// Latched when a sample reached or exceeded 0 dBFS.
    pub clipped: bool,
}

impl Default for MeterChannel {
    fn default() -> Self {
        Self {
            level_db: METER_FLOOR_DB,
            rms_db: METER_FLOOR_DB,
            hold_db: METER_FLOOR_DB,
            hold_age: 0.0,
            clipped: false,
        }
    }
}

impl MeterChannel {
    /// Integrate a new raw reading over `dt` seconds.
    pub fn update(&mut self, peak: f32, rms: f32, dt: f32) {
        let peak_db = gain_to_db(peak).max(METER_FLOOR_DB);
        let rms_db = gain_to_db(rms).max(METER_FLOOR_DB);
        let fallen = self.level_db - FALLBACK_DB_PER_S * dt;
        self.level_db = peak_db.max(fallen).max(METER_FLOOR_DB);
        // RMS: ~300 ms integration feel.
        let a = (dt / 0.3).clamp(0.0, 1.0);
        self.rms_db += (rms_db - self.rms_db) * a;
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

    /// Is anything still moving (needs redraws)?
    pub fn is_active(&self) -> bool {
        self.level_db > METER_FLOOR_DB + 0.01 || self.hold_db > METER_FLOOR_DB + 0.01
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
        self.left.update(m.left.peak, m.left.rms, dt);
        self.right.update(m.right.peak, m.right.rms, dt);
        self.count = m.count;
        for (d, r) in self.channels.iter_mut().zip(&m.channels).take(m.count) {
            d.update(r.peak, r.rms, dt);
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

    #[test]
    fn ballistics_rise_instantly_and_fall_slowly() {
        let mut m = MeterChannel::default();
        m.update(0.5, 0.3, 1.0 / 60.0);
        let l = m.level_db;
        assert!((l - (-6.02)).abs() < 0.05);
        m.update(0.0, 0.0, 0.1);
        assert!((m.level_db - (l - 2.6)).abs() < 0.01);
        assert!((m.hold_db - l).abs() < 1e-3, "hold stays");
        for _ in 0..200 {
            m.update(0.0, 0.0, 0.05);
        }
        assert_eq!(m.level_db, METER_FLOOR_DB);
        assert!(!m.is_active());
        m.update(1.2, 0.5, 0.01);
        assert!(m.clipped);
    }
}
