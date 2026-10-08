//! Stable parameter/meter slot assignment (control thread).

use faderframe_automation::AutomationTarget;
use faderframe_core::{AutomationLaneId, PluginInstanceId, SendId, TrackId};
use faderframe_project::Project;
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
    /// 1.0 = silenced because other tracks are soloed or a VCA is muted
    /// (kept apart from `mute` so mute automation never overrides them).
    pub solo_mute: ParamSlot,
    /// Linear gain of the track's VCAs whose faders are not automated
    /// (automated ones come with the timeline snapshot).
    pub vca: ParamSlot,
    /// The surround panner: x, y, z, spread, width, LFE send (dB).
    pub surround: [ParamSlot; 6],
}

const STRIP_SLOT_COUNT: u32 = 12;

/// Parameter slots of a container's chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainSlots {
    /// Linear gain; 0 while the chain is muted or another one soloed.
    pub gain: ParamSlot,
    /// Balance, −1..1.
    pub pan: ParamSlot,
}
/// Meter channels reserved per track at least (stereo).
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
    /// How many of its meter channels a track's strip uses now (the range
    /// keeps its size when a track goes back to fewer channels).
    metered: HashMap<TrackId, u16>,
    /// 1.0 while a track takes live MIDI input.
    midi_live: HashMap<TrackId, ParamSlot>,
    /// 1.0 while a MIDI track is muted (by itself, a folder it is in, or
    /// another track's solo).
    midi_mute: HashMap<TrackId, ParamSlot>,
    /// Container chains, by container and chain.
    chains: HashMap<(PluginInstanceId, usize), ChainSlots>,
    /// 1.0 while the mono check is on (listening only).
    monitor_mono: Option<ParamSlot>,
    /// The console's channel drive (dB).
    console_drive: Option<ParamSlot>,
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
            metered: HashMap::new(),
            midi_live: HashMap::new(),
            midi_mute: HashMap::new(),
            chains: HashMap::new(),
            monitor_mono: None,
            console_drive: None,
        }
    }

    /// The slots of chain `index` of `container`.
    pub fn chain(
        &mut self,
        container: PluginInstanceId,
        index: usize,
    ) -> Result<ChainSlots, SlotsExhausted> {
        if let Some(s) = self.chains.get(&(container, index)) {
            return Ok(*s);
        }
        let base = self
            .params
            .allocate(2)
            .ok_or(SlotsExhausted("parameters"))?;
        let s = ChainSlots {
            gain: ParamSlot(base),
            pan: ParamSlot(base + 1),
        };
        self.chains.insert((container, index), s);
        Ok(s)
    }

    /// The mute flag of a MIDI track.
    pub fn midi_mute(&mut self, track: TrackId) -> Result<ParamSlot, SlotsExhausted> {
        if let Some(s) = self.midi_mute.get(&track) {
            return Ok(*s);
        }
        let slot = ParamSlot(
            self.params
                .allocate(1)
                .ok_or(SlotsExhausted("parameters"))?,
        );
        self.midi_mute.insert(track, slot);
        Ok(slot)
    }

    /// The live-MIDI flag of a track.
    pub fn midi_live(&mut self, track: TrackId) -> Result<ParamSlot, SlotsExhausted> {
        if let Some(s) = self.midi_live.get(&track) {
            return Ok(*s);
        }
        let slot = ParamSlot(
            self.params
                .allocate(1)
                .ok_or(SlotsExhausted("parameters"))?,
        );
        self.midi_live.insert(track, slot);
        Ok(slot)
    }

    /// The mono check's switch (one for the session).
    pub fn monitor_mono(&mut self) -> Result<ParamSlot, SlotsExhausted> {
        if let Some(s) = self.monitor_mono {
            return Ok(s);
        }
        let s = ParamSlot(
            self.params
                .allocate(1)
                .ok_or(SlotsExhausted("parameters"))?,
        );
        self.monitor_mono = Some(s);
        Ok(s)
    }

    /// The console's channel drive (one for the session).
    pub fn console_drive(&mut self) -> Result<ParamSlot, SlotsExhausted> {
        if let Some(s) = self.console_drive {
            return Ok(s);
        }
        let s = ParamSlot(
            self.params
                .allocate(1)
                .ok_or(SlotsExhausted("parameters"))?,
        );
        self.console_drive = Some(s);
        Ok(s)
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
            solo_mute: ParamSlot(base + 4),
            vca: ParamSlot(base + 5),
            surround: std::array::from_fn(|i| ParamSlot(base + 6 + i as u32)),
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

    /// The meter channels of a track's strip, at least `channels` of them
    /// (a range too small for a new format is replaced).
    pub fn meter(&mut self, track: TrackId, channels: usize) -> Result<MeterRange, SlotsExhausted> {
        let want = (channels as u16).max(METER_CHANNELS);
        self.metered.insert(track, want);
        if let Some(m) = self.track_meters.get(&track) {
            if m.channels >= want {
                return Ok(*m);
            }
            self.meters.release(m.first, u32::from(m.channels));
            self.track_meters.remove(&track);
        }
        let first = self
            .meters
            .allocate(u32::from(want))
            .ok_or(SlotsExhausted("meters"))?;
        let m = MeterRange {
            first,
            channels: want,
        };
        self.track_meters.insert(track, m);
        Ok(m)
    }

    pub fn strip_of(&self, track: TrackId) -> Option<StripSlots> {
        self.strips.get(&track).copied()
    }

    pub fn send_of(&self, send: SendId) -> Option<ParamSlot> {
        self.sends.get(&send).copied()
    }

    pub fn meter_of(&self, track: TrackId) -> Option<MeterRange> {
        self.track_meters.get(&track).copied()
    }

    /// How many meter channels a track's strip uses now.
    pub fn metered(&self, track: TrackId) -> Option<u16> {
        self.metered.get(&track).copied()
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
        self.midi_live.retain(|t, s| {
            let keep = tracks.contains(t);
            if !keep {
                params.release(s.0, 1);
            }
            keep
        });
        self.midi_mute.retain(|t, s| {
            let keep = tracks.contains(t);
            if !keep {
                params.release(s.0, 1);
            }
            keep
        });
        let chains: HashSet<(PluginInstanceId, usize)> = project
            .tracks
            .iter()
            .flat_map(|t| {
                t.containers
                    .iter()
                    .flat_map(|(c, chains)| (0..chains.len()).map(move |i| (*c, i)))
            })
            .collect();
        self.chains.retain(|k, s| {
            let keep = chains.contains(k);
            if !keep {
                params.release(s.gain.0, 2);
            }
            keep
        });
        self.metered.retain(|t, _| tracks.contains(t));
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
        midi_live: &HashSet<TrackId>,
        suspended: &HashSet<AutomationLaneId>,
    ) -> Result<(), SlotsExhausted> {
        let solo = project.solo_audible();
        if let Some(c) = &project.console {
            let slot = self.console_drive()?;
            table.set(slot, c.drive_db as f32);
        }
        for t in &project.tracks {
            if t.input.is_midi() {
                let slot = self.midi_live(t.id)?;
                table.set(slot, if midi_live.contains(&t.id) { 1.0 } else { 0.0 });
            }
            let folder_muted = project.folder_chain(t).iter().any(|f| f.mute);
            let solo_out = solo.as_ref().is_some_and(|set| !set.contains(&t.id));
            if t.kind == faderframe_project::TrackKind::Midi {
                let slot = self.midi_mute(t.id)?;
                let muted = t.mute || folder_muted || solo_out;
                table.set(slot, if muted { 1.0 } else { 0.0 });
            }
            if !t.kind.has_audio() {
                continue;
            }
            let s = self.strip(t.id)?;
            table.set(s.volume, faderframe_core::db_to_gain(t.volume_db));
            table.set(s.pan, t.pan);
            let sp = t.surround;
            for (slot, v) in s
                .surround
                .iter()
                .zip([sp.x, sp.y, sp.z, sp.spread, sp.width, sp.lfe_db])
            {
                table.set(*slot, v);
            }
            // VCAs: static faders and mutes here, automated ones in the
            // snapshot.
            let (mut vca_gain, mut vca_muted) = (1.0f32, false);
            for v in project.vca_chain(t) {
                let lane = |target: AutomationTarget| {
                    v.automation
                        .lanes
                        .iter()
                        .any(|l| l.target == target && crate::snapshot::drives(l, suspended))
                };
                if !lane(AutomationTarget::TrackVolume) {
                    vca_gain *= faderframe_core::db_to_gain(v.volume_db);
                }
                vca_muted |= v.mute && !lane(AutomationTarget::TrackMute);
            }
            table.set(s.vca, vca_gain);
            let solo_muted = vca_muted || folder_muted || solo_out;
            table.set(s.mute, if t.mute { 1.0 } else { 0.0 });
            table.set(s.solo_mute, if solo_muted { 1.0 } else { 0.0 });
            table.set(s.phase, if t.phase_invert { 1.0 } else { 0.0 });
            for send in &t.sends {
                let slot = self.send(send.id)?;
                table.set(slot, faderframe_core::db_to_gain(send.level_db));
            }
            for (container, chains) in &t.containers {
                for (i, c) in chains.iter().enumerate() {
                    let s = self.chain(*container, i)?;
                    let heard = faderframe_project::container::audible(chains, i);
                    table.set(
                        s.gain,
                        if heard {
                            faderframe_core::db_to_gain(c.gain_db)
                        } else {
                            0.0
                        },
                    );
                    table.set(s.pan, c.pan);
                }
            }
        }
        Ok(())
    }
}
