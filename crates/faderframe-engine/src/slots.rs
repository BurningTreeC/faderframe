//! Stable parameter/meter slot assignment (control thread).

use faderframe_core::{SendId, TrackId};
use faderframe_project::{Project, TrackKind};
use faderframe_realtime::{MeterRange, ParamSlot, ParamTable, SlotAllocator};
use std::collections::{HashMap, HashSet};

/// Parameter slots of one channel strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StripSlots {
    /// Linear fader gain.
    pub volume: ParamSlot,
    /// Pan/balance, -1..1.
    pub pan: ParamSlot,
    /// 1.0 = muted (explicitly or by another track's solo).
    pub mute: ParamSlot,
    /// 1.0 = polarity inverted.
    pub phase: ParamSlot,
}

const STRIP_SLOT_COUNT: u32 = 4;
/// Meter channels reserved per track (stereo).
const METER_CHANNELS: u16 = 2;

/// Keeps slot assignments stable across graph rebuilds so the UI and
/// running processors keep addressing the same atomics.
#[derive(Debug)]
pub struct SlotRegistry {
    params: SlotAllocator,
    meters: SlotAllocator,
    strips: HashMap<TrackId, StripSlots>,
    sends: HashMap<SendId, ParamSlot>,
    track_meters: HashMap<TrackId, MeterRange>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("engine slot table exhausted ({0})")]
pub struct SlotsExhausted(pub &'static str);

impl SlotRegistry {
    pub fn new(param_capacity: u32, meter_capacity: u32) -> Self {
        Self {
            params: SlotAllocator::new(param_capacity),
            meters: SlotAllocator::new(meter_capacity),
            strips: HashMap::new(),
            sends: HashMap::new(),
            track_meters: HashMap::new(),
        }
    }

    pub fn strip(&mut self, track: TrackId) -> Result<StripSlots, SlotsExhausted> {
        if let Some(s) = self.strips.get(&track) {
            return Ok(*s);
        }
        let base = self
            .params
            .allocate(STRIP_SLOT_COUNT)
            .ok_or(SlotsExhausted("parameters"))?;
        let s = StripSlots {
            volume: ParamSlot(base),
            pan: ParamSlot(base + 1),
            mute: ParamSlot(base + 2),
            phase: ParamSlot(base + 3),
        };
        self.strips.insert(track, s);
        Ok(s)
    }

    pub fn send(&mut self, send: SendId) -> Result<ParamSlot, SlotsExhausted> {
        if let Some(s) = self.sends.get(&send) {
            return Ok(*s);
        }
        let slot = ParamSlot(
            self.params
                .allocate(1)
                .ok_or(SlotsExhausted("parameters"))?,
        );
        self.sends.insert(send, slot);
        Ok(slot)
    }

    pub fn meter(&mut self, track: TrackId) -> Result<MeterRange, SlotsExhausted> {
        if let Some(m) = self.track_meters.get(&track) {
            return Ok(*m);
        }
        let first = self
            .meters
            .allocate(METER_CHANNELS as u32)
            .ok_or(SlotsExhausted("meters"))?;
        let m = MeterRange {
            first,
            channels: METER_CHANNELS,
        };
        self.track_meters.insert(track, m);
        Ok(m)
    }

    pub fn meter_of(&self, track: TrackId) -> Option<MeterRange> {
        self.track_meters.get(&track).copied()
    }

    /// Release slots of tracks/sends that no longer exist.
    pub fn retain_project(&mut self, project: &Project) {
        let tracks: HashSet<TrackId> = project.tracks.iter().map(|t| t.id).collect();
        let sends: HashSet<SendId> = project
            .tracks
            .iter()
            .flat_map(|t| t.sends.iter().map(|s| s.id))
            .collect();
        let params = &mut self.params;
        self.strips.retain(|t, s| {
            let keep = tracks.contains(t);
            if !keep {
                params.release(s.volume.0, STRIP_SLOT_COUNT);
            }
            keep
        });
        self.sends.retain(|id, s| {
            let keep = sends.contains(id);
            if !keep {
                params.release(s.0, 1);
            }
            keep
        });
        let meters = &mut self.meters;
        self.track_meters.retain(|t, m| {
            let keep = tracks.contains(t);
            if !keep {
                meters.release(m.first, m.channels as u32);
            }
            keep
        });
    }

    /// Write the current fader/pan/mute/send values of `project` (control
    /// thread). Solo is resolved here into per-track mute flags.
    pub fn write_params(
        &mut self,
        project: &Project,
        table: &ParamTable,
    ) -> Result<(), SlotsExhausted> {
        let solo = project.solo_audible();
        for t in &project.tracks {
            if t.kind == TrackKind::Midi {
                continue;
            }
            let s = self.strip(t.id)?;
            table.set(s.volume, faderframe_core::db_to_gain(t.volume_db));
            table.set(s.pan, t.pan);
            let muted = project.effectively_muted(t, solo.as_ref());
            table.set(s.mute, if muted { 1.0 } else { 0.0 });
            table.set(s.phase, if t.phase_invert { 1.0 } else { 0.0 });
            for send in &t.sends {
                let slot = self.send(send.id)?;
                table.set(slot, faderframe_core::db_to_gain(send.level_db));
            }
        }
        Ok(())
    }
}
