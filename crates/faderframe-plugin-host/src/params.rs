use crate::{ParameterInfo, PluginError};
use faderframe_core::ParameterId;
use faderframe_realtime::AtomicF32;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// Parameter values shared between a built-in plugin's control-side
/// instance and its audio-side processor(s): written by the UI, read by the
/// processor at block start. Lock-free.
///
/// Modulation is kept apart: an offset per parameter (a share of its
/// range) that [`Self::get`] applies for the processor, while the value
/// itself — saved, automated, set from the UI — stays as set. The editors'
/// copy ([`Self::as_set`], the tap's) reads the values as set, so a drag
/// starts from the value and not from where modulation has it;
/// [`Self::live`] shows the modulation.
#[derive(Debug, Clone)]
pub struct ParamValues {
    infos: Arc<[ParameterInfo]>,
    values: Arc<[AtomicF32]>,
    mods: Arc<[AtomicF32]>,
    /// [`Self::get`] includes modulation.
    modulated: bool,
}

impl ParamValues {
    pub fn new(infos: Vec<ParameterInfo>) -> Self {
        let values = infos
            .iter()
            .map(|i| AtomicF32::new(i.default as f32))
            .collect();
        let mods = infos.iter().map(|_| AtomicF32::new(0.0)).collect();
        Self {
            infos: infos.into(),
            values,
            mods,
            modulated: true,
        }
    }

    /// The same values, read as set ([`Self::get`] without modulation).
    pub fn as_set(&self) -> Self {
        Self {
            modulated: false,
            ..self.clone()
        }
    }

    pub fn infos(&self) -> &[ParameterInfo] {
        &self.infos
    }

    pub fn index(&self, id: ParameterId) -> Option<usize> {
        self.infos.iter().position(|i| i.id == id)
    }

    /// Read by index: modulation included for processors, as set for
    /// editors ([`Self::as_set`]).
    #[inline]
    pub fn get(&self, index: usize) -> f32 {
        if self.modulated {
            self.live(index)
        } else {
            self.base(index)
        }
    }

    /// The value with its modulation now.
    #[inline]
    pub fn live(&self, index: usize) -> f32 {
        let base = self.base(index);
        let m = self
            .mods
            .get(index)
            .map_or(0.0, |v| v.load(Ordering::Relaxed));
        if m == 0.0 {
            return base;
        }
        let info = &self.infos[index];
        let (lo, hi) = (info.min, info.max);
        let v = if info.unit == crate::ParameterUnit::Hertz && lo > 0.0 && hi > lo {
            // Frequencies move in octaves, not in hertz.
            f64::from(base) * (hi / lo).powf(f64::from(m))
        } else {
            f64::from(base) + f64::from(m) * (hi - lo)
        };
        info.clamp(v) as f32
    }

    /// The value as set (no modulation).
    #[inline]
    pub fn base(&self, index: usize) -> f32 {
        self.values
            .get(index)
            .map_or(0.0, |v| v.load(Ordering::Relaxed))
    }

    /// The value as set, by id (control side: state, presets, the UI).
    pub fn get_by_id(&self, id: ParameterId) -> Option<f64> {
        self.index(id).map(|i| self.base(i) as f64)
    }

    /// A parameter's modulation (a share of its range; audio thread).
    #[inline]
    pub fn set_mod(&self, index: usize, share: f32) {
        if let Some(m) = self.mods.get(index) {
            m.store(share, Ordering::Relaxed);
        }
    }

    pub fn set_by_id(&self, id: ParameterId, value: f64) -> Result<(), PluginError> {
        let i = self.index(id).ok_or(PluginError::UnknownParameter(id))?;
        self.values[i].store(self.infos[i].clamp(value) as f32, Ordering::Relaxed);
        Ok(())
    }

    /// Apply an automation event by parameter id (audio thread).
    #[inline]
    pub fn apply_event(&self, id: ParameterId, value: f32) {
        if let Some(i) = self.infos.iter().position(|p| p.id == id) {
            let v = self.infos[i].clamp(value as f64) as f32;
            self.values[i].store(v, Ordering::Relaxed);
        }
    }

    /// Serialise values as little-endian `(u32 id, f64 value)` pairs.
    pub fn save(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.infos.len() * 12);
        for (i, info) in self.infos.iter().enumerate() {
            out.extend_from_slice(&info.id.0.to_le_bytes());
            out.extend_from_slice(&(self.base(i) as f64).to_le_bytes());
        }
        out
    }

    pub fn load(&self, data: &[u8]) -> Result<(), PluginError> {
        let (chunks, rest) = data.as_chunks::<12>();
        if !rest.is_empty() {
            return Err(PluginError::InvalidState(
                "truncated parameter block".into(),
            ));
        }
        for chunk in chunks {
            let id = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            let mut v = [0u8; 8];
            v.copy_from_slice(&chunk[4..12]);
            // Unknown ids (from newer versions) are ignored.
            let _ = self.set_by_id(ParameterId(id), f64::from_le_bytes(v));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::ParameterUnit;
    use crate::devices::param;

    #[test]
    fn modulation_moves_what_is_read_not_the_value() {
        let p = ParamValues::new(vec![
            param(0, "Gain", -60.0, 24.0, 0.0, ParameterUnit::Decibels),
            param(1, "Cutoff", 20.0, 20_000.0, 1_000.0, ParameterUnit::Hertz),
        ]);
        let saved = p.save();
        let editor = p.as_set();
        p.set_mod(0, 0.25);
        p.set_mod(1, 0.1);
        assert_eq!(p.get(0), 21.0, "a quarter of the range");
        assert_eq!(editor.get(0), 0.0, "editors read the value as set");
        assert_eq!(editor.live(0), 21.0);
        // A tenth of the range of a frequency: a tenth of its ten octaves.
        assert!((p.get(1) - 1_000.0 * 1_000f32.powf(0.1)).abs() < 0.5);
        assert_eq!(p.base(0), 0.0);
        assert_eq!(p.get_by_id(ParameterId(1)), Some(1_000.0));
        assert_eq!(p.save(), saved, "state keeps the values as set");
        // Clamped to the range; gone when cleared.
        p.set_mod(0, 1.0);
        assert_eq!(p.get(0), 24.0);
        p.set_mod(0, 0.0);
        assert_eq!(p.get(0), 0.0);
    }
}
