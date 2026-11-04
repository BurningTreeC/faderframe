//! Articulation keyswitches in the timeline snapshot (see
//! [`faderframe_project::articulation`]).
//!
//! On the control side the snapshot works out each note's articulations —
//! the directions in effect where it starts (along the track's clips in
//! time order, so a direction carries from clip to clip), its own
//! attribute over its group's — and what that state sends. A
//! [`SwitchBuilder`] notes a switch wherever a note needs another state
//! than the note before it; the lane keeps the switches ([`Switch`]) and
//! per state the events that switch ([`SwitchState`]).
//!
//! The MIDI clip player plays them on the audio thread: a switch's
//! controllers, program and keyswitch note-ons one sample before its note
//! ([`SWITCH_LEAD`]), the keyswitches' note-offs on the note's sample
//! (note-offs go first there, before the note-on). Starting or jumping
//! mid-clip *chases*: the state in effect is sent first
//! ([`ArticulationLane::at`]). Everything the audio thread reads is built
//! here, ahead; it only indexes and copies.

use faderframe_midi::MidiEvent;
use faderframe_project::articulation::{ArticulationMap, ArticulationState, Resolved};
use std::collections::HashMap;

/// Samples a switch goes out before its note.
pub const SWITCH_LEAD: i64 = 1;

/// A change of articulation state before a note.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Switch {
    /// The note's sample (the switch's note-ons go out [`SWITCH_LEAD`]
    /// before it, its note-offs on it).
    pub at: i64,
    /// Index into [`ArticulationLane::states`].
    pub state: u16,
}

/// What switching to a state sends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwitchState {
    /// Controllers, the program, then the keyswitch note-ons.
    pub on: Box<[MidiEvent]>,
    /// The keyswitches' note-offs.
    pub off: Box<[MidiEvent]>,
}

/// A lane's articulation switches, sorted by time.
#[derive(Debug, Default)]
pub struct ArticulationLane {
    pub switches: Vec<Switch>,
    pub states: Vec<SwitchState>,
}

impl ArticulationLane {
    /// The switch in effect at `pos`: the last whose note-ons go out
    /// before it (realtime-safe).
    #[inline]
    pub fn at(&self, pos: i64) -> Option<&Switch> {
        let n = self.switches.partition_point(|s| s.at - SWITCH_LEAD < pos);
        n.checked_sub(1).map(|i| &self.switches[i])
    }

    #[inline]
    pub fn state(&self, s: &Switch) -> Option<&SwitchState> {
        self.states.get(usize::from(s.state))
    }
}

/// Collects a lane's switches (control side).
#[derive(Default)]
pub(crate) struct SwitchBuilder {
    keys: HashMap<(ArticulationState, u8), u16>,
    states: Vec<SwitchState>,
    resolved: HashMap<ArticulationState, Resolved>,
    notes: Vec<(i64, u16)>,
}

impl SwitchBuilder {
    /// What `state` sends and does to notes (`map`'s resolution, kept).
    pub(crate) fn resolved(
        &mut self,
        map: &ArticulationMap,
        state: ArticulationState,
    ) -> &Resolved {
        self.resolved
            .entry(state)
            .or_insert_with(|| map.resolve(&state))
    }

    /// A note at sample `at` on `channel` (before the state's own channel)
    /// plays in `state`.
    pub(crate) fn note(
        &mut self,
        map: &ArticulationMap,
        at: i64,
        state: ArticulationState,
        channel: u8,
    ) {
        let key = (state, channel & 15);
        let index = match self.keys.get(&key) {
            Some(i) => *i,
            None => {
                let r = self.resolved(map, state).clone();
                let i = self.states.len().min(usize::from(u16::MAX)) as u16;
                self.states.push(SwitchState {
                    on: r.switch_events(channel).into_boxed_slice(),
                    off: r.releases(channel).into_boxed_slice(),
                });
                self.keys.insert(key, i);
                i
            }
        };
        self.notes.push((at, index));
    }

    /// The lane: a switch wherever a note needs another state than the
    /// one before it (`None` when no note was noted).
    pub(crate) fn finish(mut self) -> Option<Box<ArticulationLane>> {
        if self.notes.is_empty() {
            return None;
        }
        // Stable: notes at one time keep their order.
        self.notes.sort_by_key(|(at, _)| *at);
        let mut switches: Vec<Switch> = Vec::new();
        for (at, state) in self.notes {
            if switches.last().is_some_and(|s| s.state == state) {
                continue;
            }
            switches.push(Switch { at, state });
        }
        Some(Box::new(ArticulationLane {
            switches,
            states: self.states,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_core::ArticulationMapId;

    #[test]
    fn switches_come_where_the_state_changes_and_chase_finds_them() {
        let map = ArticulationMap::template(ArticulationMapId(1), "brass").unwrap();
        let sus = map.find("Sustain").unwrap().id;
        let stacc = map.find("Staccato").unwrap().id;
        let plain = map.default_state();
        let short = map.state(&plain, Some(stacc));
        assert_eq!(plain[0], Some(sus));
        let mut b = SwitchBuilder::default();
        for (at, s) in [(100, plain), (200, plain), (300, short), (400, plain)] {
            b.note(&map, at, s, 2);
        }
        let lane = b.finish().unwrap();
        let ats: Vec<i64> = lane.switches.iter().map(|s| s.at).collect();
        assert_eq!(ats, [100, 300, 400], "no switch where the state stays");
        assert_eq!(lane.states.len(), 2);
        let first = lane.state(&lane.switches[0]).unwrap();
        assert!(matches!(
            first.on[0],
            MidiEvent::NoteOn {
                channel: 2,
                key: 24,
                ..
            }
        ));
        assert!(matches!(
            first.off[0],
            MidiEvent::NoteOff {
                channel: 2,
                key: 24,
                ..
            }
        ));
        // Chasing: before the first switch's note-ons, nothing; from the
        // sample they go out on, that switch.
        assert_eq!(lane.at(99), None);
        assert_eq!(lane.at(100).map(|s| s.at), Some(100));
        assert_eq!(lane.at(299).map(|s| s.at), Some(100));
        assert_eq!(lane.at(300).map(|s| s.at), Some(300));
        assert_eq!(lane.at(10_000).map(|s| s.at), Some(400));
        assert!(SwitchBuilder::default().finish().is_none());
    }
}
