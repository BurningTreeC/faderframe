//! Containers: a device (`faderframe.container`, an ordinary insert slot)
//! that splits its input into parallel chains — each a series of devices
//! with its own level, pan, mute and solo — and mixes them back together.
//! A chain without devices is the dry signal. The chains live in
//! `Track::containers` by the container's slot (containers in containers
//! there too), so a container slot is like any other: moved, bypassed,
//! removed (and restored by undo) with its chains.

use crate::PluginSlot;
use serde::{Deserialize, Serialize};

/// Most chains in a container.
pub const MAX_CHAINS: usize = 16;
/// How deep containers nest.
pub const MAX_DEPTH: usize = 4;

/// One of a container's parallel chains.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Chain {
    pub name: String,
    #[serde(default)]
    pub inserts: Vec<PluginSlot>,
    #[serde(default)]
    pub gain_db: f32,
    /// −1 (left) … 1 (right).
    #[serde(default)]
    pub pan: f32,
    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub solo: bool,
}

impl Chain {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            inserts: Vec::new(),
            gain_db: 0.0,
            pan: 0.0,
            mute: false,
            solo: false,
        }
    }
}

/// Is chain `i` heard (not muted, and soloed when any chain is)?
pub fn audible(chains: &[Chain], i: usize) -> bool {
    chains
        .get(i)
        .is_some_and(|c| !c.mute && (c.solo || !chains.iter().any(|x| x.solo)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solo_and_mute_decide_what_is_heard() {
        let mut chains = vec![Chain::new("Dry"), Chain::new("Wet"), Chain::new("Crush")];
        assert!((0..3).all(|i| audible(&chains, i)));
        chains[1].mute = true;
        assert!(!audible(&chains, 1));
        chains[2].solo = true;
        assert_eq!(
            (0..3).map(|i| audible(&chains, i)).collect::<Vec<_>>(),
            [false, false, true]
        );
        assert!(!audible(&chains, 3));
    }
}
