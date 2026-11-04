//! Glides of mixer parameters on the audio thread: a mix scene's
//! crossfade, a morph between scenes, the switch between the sides of a
//! scene compare (see `faderframe_project::scenes`).
//!
//! A [`Glide`] moves parameter slots — faders, pans, send and cue levels,
//! the surround panner's positions, the console's drive — from where they
//! are when the audio thread takes it to their targets over a number of
//! frames. Before every chunk of a callback it writes each slot's value
//! for the chunk's *end*; the strips and sends ramp linearly across each
//! block to the value they read, so the ramps join into one unbroken line
//! however the user interface's ticks fall (no steps of a tick, no
//! zipper). Gains move in fader travel (the console's law), as a
//! motorised fader would; everything else linearly. At the end the exact
//! targets are written and the glide goes back to the control thread.
//!
//! While a glide holds slots the controller writes nothing into them
//! ([`crate::EngineController::glide_mix`] keeps their set; parameter
//! syncs skip it) until the session releases them — at once (a stop) or
//! once the audio thread has finished (the targets are the project's
//! values by then, so nothing jumps). Switched values (mutes, bypass,
//! pan modes) are not glided: the session sets them at the fade's switch
//! point as ordinary edits. Realtime-safe: no allocation, the fader law's
//! table is made on the control thread.

use faderframe_core::{FaderLaw, db_to_gain, gain_to_db};
use faderframe_realtime::{ParamSlot, ParamTable};

/// How a slot's value moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GlideLaw {
    /// A linear gain, moved in fader travel.
    Gain,
    /// Moved linearly (pans, positions, the drive in dB).
    Linear,
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    slot: ParamSlot,
    law: GlideLaw,
    /// The target as the slot holds it (written exactly at the end).
    target: f32,
    /// Start and target in the moving domain (travel for gains).
    from: f32,
    to: f32,
}

/// Slots on their way to targets (see the module docs).
pub(crate) struct Glide {
    pub(crate) id: u64,
    entries: Box<[Entry]>,
    frames: u64,
    elapsed: u64,
    law: FaderLaw,
}

impl Glide {
    /// A glide of `entries` (slot, law, target) over `frames` (control
    /// thread).
    pub(crate) fn new(id: u64, entries: &[(ParamSlot, GlideLaw, f32)], frames: u64) -> Self {
        let law = FaderLaw::console();
        let entries = entries
            .iter()
            .map(|&(slot, glide, target)| Entry {
                slot,
                law: glide,
                target,
                from: f32::NAN,
                to: domain(&law, glide, target),
            })
            .collect();
        Self {
            id,
            entries,
            frames: frames.max(1),
            elapsed: 0,
            law,
        }
    }

    /// The slots it moves.
    pub(crate) fn slots(&self) -> impl Iterator<Item = ParamSlot> + '_ {
        self.entries.iter().map(|e| e.slot)
    }

    /// Start from the slots' values now (audio thread, when it takes the
    /// glide; a value that cannot move — not a number — starts at its
    /// target).
    pub(crate) fn begin(&mut self, table: &ParamTable) {
        for e in self.entries.iter_mut() {
            let from = domain(&self.law, e.law, table.get(e.slot));
            e.from = if from.is_finite() && e.to.is_finite() {
                from
            } else {
                e.to
            };
        }
        self.elapsed = 0;
    }

    /// Move on by `frames`, writing the values for the end of them; true
    /// once the targets are written (audio thread).
    pub(crate) fn advance(&mut self, frames: usize, table: &ParamTable) -> bool {
        self.elapsed = (self.elapsed + frames as u64).min(self.frames);
        if self.elapsed >= self.frames {
            for e in self.entries.iter() {
                table.set(e.slot, e.target);
            }
            return true;
        }
        let t = self.elapsed as f64 / self.frames as f64;
        for e in self.entries.iter() {
            let x = (f64::from(e.from) + (f64::from(e.to) - f64::from(e.from)) * t) as f32;
            table.set(e.slot, value(&self.law, e.law, x));
        }
        false
    }

    /// How far it is (0…1).
    pub(crate) fn progress(&self) -> f32 {
        (self.elapsed as f64 / self.frames as f64) as f32
    }
}

/// A slot's value in the domain it moves in.
fn domain(law: &FaderLaw, glide: GlideLaw, v: f32) -> f32 {
    match glide {
        GlideLaw::Gain if v.is_nan() => f32::NAN,
        GlideLaw::Gain => law.db_to_position(gain_to_db(v.max(0.0))),
        GlideLaw::Linear => v,
    }
}

fn value(law: &FaderLaw, glide: GlideLaw, x: f32) -> f32 {
    match glide {
        GlideLaw::Gain if x <= 0.0 => 0.0,
        GlideLaw::Gain => db_to_gain(law.position_to_db(x)),
        GlideLaw::Linear => x,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_glide_moves_gains_in_travel_and_ends_exactly() {
        let table = ParamTable::new(4);
        table.set(ParamSlot(0), 1.0);
        table.set(ParamSlot(1), -1.0);
        table.set(ParamSlot(2), f32::NAN);
        let target = db_to_gain(-30.0);
        let mut g = Glide::new(
            7,
            &[
                (ParamSlot(0), GlideLaw::Gain, target),
                (ParamSlot(1), GlideLaw::Linear, 0.5),
                (ParamSlot(2), GlideLaw::Linear, 0.25),
            ],
            1000,
        );
        g.begin(&table);
        let law = FaderLaw::console();
        let mut last = f32::INFINITY;
        let mut steps = Vec::new();
        for _ in 0..9 {
            assert!(!g.advance(100, &table));
            let pos = law.db_to_position(gain_to_db(table.get(ParamSlot(0))));
            steps.push(last - pos);
            last = pos;
        }
        // Even steps of travel (the first from the start).
        for s in &steps[2..] {
            assert!((s - steps[1]).abs() < 1e-4, "{steps:?}");
        }
        assert!((table.get(ParamSlot(1)) - 0.35).abs() < 1e-5);
        // Not a number to start with: at its target at once.
        assert_eq!(table.get(ParamSlot(2)), 0.25);
        assert!((g.progress() - 0.9).abs() < 1e-6);
        assert!(g.advance(500, &table));
        assert_eq!(table.get(ParamSlot(0)), target);
        assert_eq!(table.get(ParamSlot(1)), 0.5);
        assert_eq!(g.progress(), 1.0);
    }

    #[test]
    fn silence_glides_from_the_bottom_of_the_fader() {
        let table = ParamTable::new(1);
        table.set(ParamSlot(0), 0.0);
        let mut g = Glide::new(1, &[(ParamSlot(0), GlideLaw::Gain, 1.0)], 100);
        g.begin(&table);
        g.advance(1, &table);
        let early = table.get(ParamSlot(0));
        assert!(early > 0.0 && early < 1e-3, "{early}");
        g.advance(98, &table);
        assert!(table.get(ParamSlot(0)) < 1.0);
        assert!(g.advance(1, &table));
        assert_eq!(table.get(ParamSlot(0)), 1.0);
    }
}
