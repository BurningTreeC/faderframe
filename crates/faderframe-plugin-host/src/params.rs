use crate::{ParameterInfo, PluginError};
use faderframe_core::ParameterId;
use faderframe_realtime::AtomicF32;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// Parameter values shared between a built-in plugin's control-side
/// instance and its audio-side processor(s): written by the UI, read by the
/// processor at block start. Lock-free.
#[derive(Debug, Clone)]
pub struct ParamValues {
    infos: Arc<[ParameterInfo]>,
    values: Arc<[AtomicF32]>,
}

impl ParamValues {
    pub fn new(infos: Vec<ParameterInfo>) -> Self {
        let values = infos
            .iter()
            .map(|i| AtomicF32::new(i.default as f32))
            .collect();
        Self {
            infos: infos.into(),
            values,
        }
    }

    pub fn infos(&self) -> &[ParameterInfo] {
        &self.infos
    }

    fn index(&self, id: ParameterId) -> Option<usize> {
        self.infos.iter().position(|i| i.id == id)
    }

    /// Read by index (audio thread).
    #[inline]
    pub fn get(&self, index: usize) -> f32 {
        self.values
            .get(index)
            .map_or(0.0, |v| v.load(Ordering::Relaxed))
    }

    pub fn get_by_id(&self, id: ParameterId) -> Option<f64> {
        self.index(id).map(|i| self.get(i) as f64)
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
            out.extend_from_slice(&(self.get(i) as f64).to_le_bytes());
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
