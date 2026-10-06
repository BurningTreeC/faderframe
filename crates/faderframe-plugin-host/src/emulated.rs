//! Modulation for plugins whose parameters have none of their own (VST3,
//! Audio Units): the processor sends the modulated value as an ordinary
//! parameter change and keeps the value as set — the *base* — to itself.
//!
//! [`EmulatedMods`] lives with the processor (audio thread): it follows
//! every value set (the UI, automation, mapped MIDI, the plugin's own
//! changes of parameters that are not modulated), adds each block's
//! modulation to the base, and sends the base again once a parameter's
//! modulation ends. The bases of the parameters being modulated are
//! published in [`ModBases`] so the instance can save them with the
//! plugin's state, which holds the modulated values. Nothing here
//! allocates after [`EmulatedMods::new`].

use crate::ParamMod;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Parameters modulated at once (the engine's `MAX_PARAM_MODS`).
pub const SLOTS: usize = 64;
const EMPTY: u32 = u32::MAX;

/// The bases of the parameters being modulated (for saving).
pub struct ModBases {
    ids: [AtomicU32; SLOTS],
    values: [AtomicU64; SLOTS],
}

impl Default for ModBases {
    fn default() -> Self {
        Self {
            ids: std::array::from_fn(|_| AtomicU32::new(EMPTY)),
            values: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

impl ModBases {
    /// The parameters modulated now and their values as set.
    pub fn snapshot(&self) -> Vec<(u32, f64)> {
        (0..SLOTS)
            .filter_map(|i| {
                let id = self.ids[i].load(Ordering::Acquire);
                (id != EMPTY).then(|| (id, f64::from_bits(self.values[i].load(Ordering::Relaxed))))
            })
            .collect()
    }

    fn publish(&self, i: usize, id: u32, value: f64) {
        self.values[i].store(value.to_bits(), Ordering::Relaxed);
        self.ids[i].store(id, Ordering::Release);
    }

    fn clear_from(&self, i: usize) {
        for id in &self.ids[i..] {
            id.store(EMPTY, Ordering::Release);
        }
    }
}

/// The audio thread's side (see the module docs).
pub struct EmulatedMods {
    /// Every parameter's value as set, by id.
    values: Box<[(u32, f64)]>,
    /// Modulated in the last block: id and the value sent.
    active: [(u32, f64); SLOTS],
    count: usize,
    next: [(u32, f64); SLOTS],
    bases: Arc<ModBases>,
}

impl EmulatedMods {
    /// For parameters with these values (allocates; control thread).
    pub fn new(mut values: Vec<(u32, f64)>, bases: Arc<ModBases>) -> Self {
        values.sort_unstable_by_key(|(id, _)| *id);
        values.dedup_by_key(|(id, _)| *id);
        bases.clear_from(0);
        Self {
            values: values.into_boxed_slice(),
            active: [(EMPTY, 0.0); SLOTS],
            count: 0,
            next: [(EMPTY, 0.0); SLOTS],
            bases,
        }
    }

    /// A parameter was set to `value` (the UI, automation, a preset…).
    #[inline]
    pub fn set(&mut self, id: u32, value: f64) {
        if let Ok(i) = self.values.binary_search_by_key(&id, |(p, _)| *p) {
            self.values[i].1 = value;
        }
    }

    /// The value as set.
    #[inline]
    pub fn base(&self, id: u32) -> Option<f64> {
        self.values
            .binary_search_by_key(&id, |(p, _)| *p)
            .ok()
            .map(|i| self.values[i].1)
    }

    /// Was the parameter modulated in the last block (its reported changes
    /// are then the modulated values, not new bases)?
    #[inline]
    pub fn modulated(&self, id: u32) -> bool {
        self.active[..self.count].iter().any(|(p, _)| *p == id)
    }

    /// This block's modulation: `send` gets every modulated parameter's
    /// value (`clamp(base + delta)`, `delta` read off its [`ParamMod`])
    /// and the base of every parameter whose modulation ended.
    pub fn block(
        &mut self,
        mods: &[ParamMod],
        delta: impl Fn(&ParamMod) -> f64,
        clamp: impl Fn(u32, f64) -> f64,
        mut send: impl FnMut(u32, f64),
    ) {
        let mut n = 0;
        for m in mods {
            let id = m.parameter.0;
            if n == SLOTS || self.next[..n].iter().any(|(p, _)| *p == id) {
                continue;
            }
            let Some(base) = self.base(id) else { continue };
            let value = clamp(id, base + delta(m));
            let last = self.active[..self.count]
                .iter()
                .find(|(p, _)| *p == id)
                .map(|(_, v)| *v);
            if last != Some(value) {
                send(id, value);
            }
            self.next[n] = (id, value);
            self.bases.publish(n, id, base);
            n += 1;
        }
        for &(id, _) in &self.active[..self.count] {
            if !self.next[..n].iter().any(|(p, _)| *p == id)
                && let Some(base) = self.base(id)
            {
                send(id, base);
            }
        }
        if n < self.count {
            self.bases.clear_from(n);
        }
        std::mem::swap(&mut self.active, &mut self.next);
        self.count = n;
    }
}

/// The modulation of `id` this block, if any (`delta` read off it).
#[inline]
pub fn delta_of(mods: &[ParamMod], id: u32, delta: impl Fn(&ParamMod) -> f64) -> Option<f64> {
    mods.iter().find(|m| m.parameter.0 == id).map(delta)
}

/// A state with the bases of modulated parameters after it (`FFMB`, count,
/// then id and value pairs; readers that do not know it ignore it).
pub fn append_bases(state: &mut Vec<u8>, bases: &[(u32, f64)]) {
    if bases.is_empty() {
        return;
    }
    state.extend_from_slice(TRAILER);
    state.extend_from_slice(&(bases.len() as u32).to_le_bytes());
    for (id, v) in bases {
        state.extend_from_slice(&id.to_le_bytes());
        state.extend_from_slice(&v.to_le_bytes());
    }
}

/// The bases [`append_bases`] wrote at `rest` (the bytes after the
/// plugin's own state), if it holds them.
pub fn read_bases(rest: &[u8]) -> Vec<(u32, f64)> {
    let Some(body) = rest.strip_prefix(TRAILER) else {
        return Vec::new();
    };
    let Some(n) = body
        .get(..4)
        .and_then(|b| b.try_into().ok())
        .map(u32::from_le_bytes)
    else {
        return Vec::new();
    };
    body[4..]
        .as_chunks::<12>()
        .0
        .iter()
        .take(n as usize)
        .filter_map(|c| {
            let id = u32::from_le_bytes(c[..4].try_into().ok()?);
            let v = f64::from_le_bytes(c[4..].try_into().ok()?);
            v.is_finite().then_some((id, v))
        })
        .collect()
}

const TRAILER: &[u8; 4] = b"FFMB";

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use faderframe_core::ParameterId;

    fn m(id: u32, share: f32) -> ParamMod {
        ParamMod {
            parameter: ParameterId(id),
            share,
            amount: share,
        }
    }

    #[test]
    fn modulation_rides_on_the_base_and_ends_on_it() {
        let bases = Arc::new(ModBases::default());
        let mut e = EmulatedMods::new(vec![(7, 0.5), (3, 0.2)], Arc::clone(&bases));
        let mut sent = Vec::new();
        let clamp = |_: u32, v: f64| v.clamp(0.0, 1.0);
        let share = |m: &ParamMod| f64::from(m.share);
        e.block(&[m(7, 0.25)], share, clamp, |id, v| sent.push((id, v)));
        assert_eq!(sent, vec![(7, 0.75)]);
        assert!(e.modulated(7) && !e.modulated(3));
        assert_eq!(bases.snapshot(), vec![(7, 0.5)]);
        // The same value again: nothing to send; a new base moves it.
        sent.clear();
        e.block(&[m(7, 0.25)], share, clamp, |id, v| sent.push((id, v)));
        assert!(sent.is_empty());
        e.set(7, 0.9);
        e.block(&[m(7, 0.25)], share, clamp, |id, v| sent.push((id, v)));
        assert_eq!(sent, vec![(7, 1.0)], "clamped");
        // Ended: the base again, and nothing published.
        sent.clear();
        e.block(&[], share, clamp, |id, v| sent.push((id, v)));
        assert_eq!(sent, vec![(7, 0.9)]);
        assert!(bases.snapshot().is_empty());
        assert!(!e.modulated(7));
        // Unknown parameters are left alone.
        e.block(&[m(99, 0.1)], share, clamp, |id, v| sent.push((id, v)));
        assert_eq!(sent.len(), 1);
    }

    #[test]
    fn bases_travel_after_a_state() {
        let mut state = b"plugin".to_vec();
        append_bases(&mut state, &[(4, 0.25), (9, -3.5)]);
        assert_eq!(read_bases(&state[6..]), vec![(4, 0.25), (9, -3.5)]);
        assert!(read_bases(b"").is_empty());
        assert!(read_bases(b"other").is_empty());
    }
}
