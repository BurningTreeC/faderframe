//! Stable parameter/meter slot assignment (control thread).

use faderframe_automation::AutomationTarget;
use faderframe_core::surround::SURROUND_VALUES;
use faderframe_core::{AutomationLaneId, CueId, PluginInstanceId, SendId, TrackId};
use faderframe_project::Project;
use faderframe_realtime::{MeterRange, ParamSlot, ParamTable, SlotAllocator};
use std::collections::{HashMap, HashSet};

/// Parameter slots of one channel strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StripSlots {
    /// Linear fader gain.
    pub volume: ParamSlot,
    /// Mono pan / stereo balance, -1..1.
    pub pan: ParamSlot,
    /// 1.0 = a stereo source is dual-panned (0.0: balanced).
    pub pan_mode: ParamSlot,
    /// Dual pan: the left input channel's position, -1..1.
    pub pan_left: ParamSlot,
    /// Dual pan: the right input channel's position, -1..1.
    pub pan_right: ParamSlot,
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
    /// The surround panner: its values in
    /// [`faderframe_core::SurroundPan::values`]'s order.
    pub surround: [ParamSlot; SURROUND_VALUES],
}

const STRIP_SLOT_COUNT: u32 = 9 + SURROUND_VALUES as u32;

/// Parameter slots every strip and fold reads (one set per session).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MixSlots {
    /// The project's pan law ([`faderframe_core::PanLaw::index`]).
    pub pan_law: ParamSlot,
    /// How beds fold into stereo and mono
    /// ([`faderframe_core::surround::Downmix::values`]).
    pub downmix: [ParamSlot; 5],
}

impl MixSlots {
    /// The downmix as the slots say.
    pub fn downmix(&self, params: &ParamTable) -> faderframe_core::surround::Downmix {
        faderframe_core::surround::Downmix::from_values(&self.downmix.map(|s| params.get(s)))
    }
}

/// Parameter slots of a track's send to a cue mix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CueSendSlots {
    /// Linear gain of its level.
    pub gain: ParamSlot,
    /// −1…1 (a mono tap by the pan law, a stereo one's balance).
    pub pan: ParamSlot,
    /// 1.0 while the send is on.
    pub on: ParamSlot,
}

/// Parameter slots of a cue mix's talkback: the microphone's gain into it
/// and how much the mix dims while the engineer talks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CueTalkSlots {
    /// Linear gain of the talkback microphone into this cue mix (0: not
    /// talking, or the cue mix does not hear it).
    pub talk: ParamSlot,
    /// Linear gain the cue mix is dimmed by while talking (1: not).
    pub dim: ParamSlot,
}

/// The talkback microphone as the engine has it (a session setting, not
/// the project's): switched on, its device input, its gain, the dim.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Talkback {
    pub on: bool,
    /// The device input it comes from (`None`: no talkback at all).
    pub input: Option<u16>,
    /// Linear gain of the microphone into the cue mixes that hear it.
    pub level: f32,
    /// Linear gain the cue mixes hearing it play at while talking.
    pub dim: f32,
}

impl Default for Talkback {
    fn default() -> Self {
        Self {
            on: false,
            input: None,
            level: 1.0,
            dim: faderframe_core::db_to_gain(-20.0),
        }
    }
}

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
    /// Sends' pans (NaN: unpanned).
    send_pans: HashMap<SendId, ParamSlot>,
    /// Tracks' sends to cue mixes.
    cue_sends: HashMap<(TrackId, CueId), CueSendSlots>,
    /// The cue mixes' talkback slots.
    cue_talk: HashMap<CueId, CueTalkSlots>,
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
    /// The pan law and the downmix.
    mix: Option<MixSlots>,
    /// Bass management of the speaker monitoring (listening only).
    bass: Option<[ParamSlot; 4]>,
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
            send_pans: HashMap::new(),
            cue_sends: HashMap::new(),
            cue_talk: HashMap::new(),
            track_meters: HashMap::new(),
            metered: HashMap::new(),
            midi_live: HashMap::new(),
            midi_mute: HashMap::new(),
            chains: HashMap::new(),
            monitor_mono: None,
            console_drive: None,
            mix: None,
            bass: None,
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

    /// Bass management of the speaker monitoring (one set for the
    /// session; [`faderframe_core::surround::BassManagement::values`]).
    pub fn bass_management(&mut self) -> Result<[ParamSlot; 4], SlotsExhausted> {
        if let Some(s) = self.bass {
            return Ok(s);
        }
        let base = self
            .params
            .allocate(4)
            .ok_or(SlotsExhausted("parameters"))?;
        let s = std::array::from_fn(|i| ParamSlot(base + i as u32));
        self.bass = Some(s);
        Ok(s)
    }

    /// The pan law and the downmix (one set for the session).
    pub fn mix(&mut self) -> Result<MixSlots, SlotsExhausted> {
        if let Some(s) = self.mix {
            return Ok(s);
        }
        let base = self
            .params
            .allocate(6)
            .ok_or(SlotsExhausted("parameters"))?;
        let s = MixSlots {
            pan_law: ParamSlot(base),
            downmix: std::array::from_fn(|i| ParamSlot(base + 1 + i as u32)),
        };
        self.mix = Some(s);
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
            pan_mode: ParamSlot(base + 6),
            pan_left: ParamSlot(base + 7),
            pan_right: ParamSlot(base + 8),
            surround: std::array::from_fn(|i| ParamSlot(base + 9 + i as u32)),
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

    /// The pan of a send (NaN while it is unpanned).
    pub fn send_pan(&mut self, send: SendId) -> Result<ParamSlot, SlotsExhausted> {
        if let Some(s) = self.send_pans.get(&send) {
            return Ok(*s);
        }
        let slot = ParamSlot(
            self.params
                .allocate(1)
                .ok_or(SlotsExhausted("parameters"))?,
        );
        self.send_pans.insert(send, slot);
        Ok(slot)
    }

    /// The slots of `track`'s send to cue mix `cue`.
    pub fn cue_send(&mut self, track: TrackId, cue: CueId) -> Result<CueSendSlots, SlotsExhausted> {
        if let Some(s) = self.cue_sends.get(&(track, cue)) {
            return Ok(*s);
        }
        let base = self
            .params
            .allocate(3)
            .ok_or(SlotsExhausted("parameters"))?;
        let s = CueSendSlots {
            gain: ParamSlot(base),
            pan: ParamSlot(base + 1),
            on: ParamSlot(base + 2),
        };
        self.cue_sends.insert((track, cue), s);
        Ok(s)
    }

    pub fn cue_send_of(&self, track: TrackId, cue: CueId) -> Option<CueSendSlots> {
        self.cue_sends.get(&(track, cue)).copied()
    }

    /// The talkback slots of cue mix `cue`.
    pub fn cue_talk(&mut self, cue: CueId) -> Result<CueTalkSlots, SlotsExhausted> {
        if let Some(s) = self.cue_talk.get(&cue) {
            return Ok(*s);
        }
        let base = self
            .params
            .allocate(2)
            .ok_or(SlotsExhausted("parameters"))?;
        let s = CueTalkSlots {
            talk: ParamSlot(base),
            dim: ParamSlot(base + 1),
        };
        self.cue_talk.insert(cue, s);
        Ok(s)
    }

    /// The talkback's gains into every cue mix (and their dims) from the
    /// session's talkback state and which cue mixes hear it.
    pub fn write_talkback(
        &mut self,
        project: &Project,
        table: &ParamTable,
        talk: &Talkback,
    ) -> Result<(), SlotsExhausted> {
        for c in &project.cue_mixes {
            let s = self.cue_talk(c.id)?;
            let talking = talk.on && talk.input.is_some() && c.talkback;
            table.set(s.talk, if talking { talk.level.max(0.0) } else { 0.0 });
            table.set(
                s.dim,
                if talking {
                    talk.dim.clamp(0.0, 1.0)
                } else {
                    1.0
                },
            );
        }
        Ok(())
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
        // A cue mix's own channel (its master strip, meters) is keyed like
        // a track's.
        let tracks: HashSet<TrackId> = project
            .tracks
            .iter()
            .map(|t| t.id)
            .chain(project.cue_mixes.iter().map(|c| c.channel()))
            .collect();
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
        self.send_pans.retain(|id, s| {
            let keep = sends.contains(id);
            if !keep {
                params.release(s.0, 1);
            }
            keep
        });
        let cues: HashSet<CueId> = project.cue_mixes.iter().map(|c| c.id).collect();
        self.cue_sends.retain(|(t, c), s| {
            let keep = tracks.contains(t) && cues.contains(c);
            if !keep {
                params.release(s.gain.0, 3);
            }
            keep
        });
        self.cue_talk.retain(|c, s| {
            let keep = cues.contains(c);
            if !keep {
                params.release(s.talk.0, 2);
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
        let mix = self.mix()?;
        table.set(mix.pan_law, project.pan_law.index() as f32);
        for (slot, v) in mix.downmix.iter().zip(project.downmix.values()) {
            table.set(*slot, v);
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
            let dual = t.pan_mode == faderframe_core::PanMode::Dual;
            table.set(s.pan_mode, if dual { 1.0 } else { 0.0 });
            table.set(s.pan_left, t.dual_pan.left);
            table.set(s.pan_right, t.dual_pan.right);
            for (slot, v) in s.surround.iter().zip(t.surround.values()) {
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
                let pan = self.send_pan(send.id)?;
                table.set(pan, send.pan.unwrap_or(f32::NAN));
            }
            // Every cue mix it feeds (a send it has none of is silent).
            if faderframe_project::cue::feeds_cues(t.kind) {
                for c in &project.cue_mixes {
                    let s = self.cue_send(t.id, c.id)?;
                    let send = t.cue_send(c.id);
                    let gain = send.map_or(0.0, |s| faderframe_core::db_to_gain(s.level_db));
                    table.set(s.gain, gain);
                    table.set(s.pan, send.map_or(0.0, |s| s.pan));
                    table.set(s.on, if send.is_some_and(|s| s.on) { 1.0 } else { 0.0 });
                }
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
        // The cue mixes' own channels: their level and mute (nothing else
        // of a strip moves there).
        for c in &project.cue_mixes {
            let s = self.strip(c.channel())?;
            table.set(s.volume, faderframe_core::db_to_gain(c.level_db));
            table.set(s.mute, if c.mute { 1.0 } else { 0.0 });
            table.set(s.pan, 0.0);
            table.set(s.pan_mode, 0.0);
            table.set(s.pan_left, -1.0);
            table.set(s.pan_right, 1.0);
            table.set(s.phase, 0.0);
            table.set(s.solo_mute, 0.0);
            table.set(s.vca, 1.0);
            for (slot, v) in s
                .surround
                .iter()
                .zip(faderframe_core::SurroundPan::default().values())
            {
                table.set(*slot, v);
            }
        }
        Ok(())
    }
}
