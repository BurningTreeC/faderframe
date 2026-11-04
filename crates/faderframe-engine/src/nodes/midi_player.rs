use crate::articulation::{ArticulationLane, SWITCH_LEAD};
use crate::context::EngineContext;
use crate::launch::{self, Play};
use crate::snapshot::Lane;
use faderframe_audio_graph::{NodeIo, ProcessContext, Processor};
use faderframe_core::TrackId;
use faderframe_midi::{Fine, MidiBuffer, MidiEvent, NoteTracker, TimedMidiEvent};
use faderframe_realtime::ParamSlot;

/// Controller slots per channel: 128 CCs, pitch bend, channel pressure,
/// program (chased after the CCs, so a bank select comes first).
const SLOTS: usize = 131;
const PB: usize = 128;
const AT: usize = 129;
const PC: usize = 130;

/// A chased controller's key in [`crate::snapshot::ChasePoints`].
pub(crate) fn chase_key(ev: MidiEvent) -> Option<u16> {
    slot(ev).map(|(ch, i)| (ch * SLOTS + i) as u16)
}

fn slot(ev: MidiEvent) -> Option<(usize, usize)> {
    match ev {
        MidiEvent::ControlChange {
            channel,
            controller,
            ..
        } => Some(((channel & 15) as usize, (controller & 127) as usize)),
        MidiEvent::PitchBend { channel, .. } => Some(((channel & 15) as usize, PB)),
        MidiEvent::ChannelPressure { channel, .. } => Some(((channel & 15) as usize, AT)),
        MidiEvent::ProgramChange { channel, .. } => Some(((channel & 15) as usize, PC)),
        _ => None,
    }
}

/// Emits the MIDI events of one track's MIDI regions with exact sample
/// offsets.
///
/// On every transport discontinuity (stop, locate, loop wrap) the notes it
/// started are released at offset 0, so nothing hangs; controllers are
/// *chased*: playback starting mid-clip first sends the value each
/// controller has at that point (a held sustain pedal, a bend, the mod
/// wheel, the program), and controllers the player moved go back to rest (sustain off,
/// bend centred) when playback jumps or stops. Articulation switches
/// (keyswitches, programs, controllers; [`crate::articulation`]) go out
/// before their notes and are chased the same way. All state is fixed-size.
pub struct MidiClipPlayer {
    track: TrackId,
    /// A MIDI track's mute: while set, it plays nothing (its sounding
    /// notes are released, controllers chased again when it comes back).
    mute: Option<ParamSlot>,
    muted: bool,
    tracker: NoteTracker,
    /// Value last sent per channel and controller slot.
    sent: Box<ChannelValues>,
    /// Scratch for chasing.
    chase: Box<ChannelValues>,
    was_playing: bool,
    /// What played last (the arrangement or a launched clip).
    playing: Play,
    /// Keyswitch note-offs a chase could not send in its block (a block
    /// one frame long), sent at the start of the next.
    pending_offs: [Option<MidiEvent>; PENDING_OFFS],
}

/// A value per channel and controller slot, with its MIDI 2.0 resolution.
type ChannelValues = [[Option<(MidiEvent, Fine)>; SLOTS]; 16];

/// Keyswitch note-offs kept for the next block.
const PENDING_OFFS: usize = 16;

impl MidiClipPlayer {
    pub fn new(track: TrackId) -> Self {
        Self {
            track,
            mute: None,
            muted: false,
            tracker: NoteTracker::default(),
            sent: Box::new([[None; SLOTS]; 16]),
            chase: Box::new([[None; SLOTS]; 16]),
            was_playing: false,
            playing: Play::Arrangement,
            pending_offs: [None; PENDING_OFFS],
        }
    }

    /// Silent while the slot is set (MIDI tracks' mute).
    pub fn with_mute(mut self, mute: ParamSlot) -> Self {
        self.mute = Some(mute);
        self
    }

    /// Put moved controllers back to rest (pedals up, bend centred).
    fn rest(&mut self, out: &mut MidiBuffer) {
        for (ch, slots) in self.sent.iter_mut().enumerate() {
            for (i, s) in slots.iter_mut().enumerate() {
                let Some((ev, _)) = s.take() else { continue };
                let rest = match (i, ev) {
                    (PB, _) => MidiEvent::PitchBend {
                        channel: ch as u8,
                        value: 8192,
                    },
                    (AT, _) => MidiEvent::ChannelPressure {
                        channel: ch as u8,
                        pressure: 0,
                    },
                    // Switches (sustain, sostenuto, soft pedal, …) off; other
                    // CCs keep their value (volume, pan, mod wheel …).
                    (64..=69, _) => MidiEvent::ControlChange {
                        channel: ch as u8,
                        controller: i as u8,
                        value: 0,
                    },
                    _ => continue,
                };
                let _ = out.push(TimedMidiEvent::new(0, rest));
            }
        }
    }

    /// Announce an MPE zone: the MPE configuration message (RPN 6) on the
    /// master channel and the pitch bend range (RPN 0) of every member
    /// channel. Sent whenever playback starts.
    fn announce_mpe(cfg: faderframe_project::MpeConfig, out: &mut MidiBuffer) {
        let cc = |channel: u8, controller: u8, value: u8| MidiEvent::ControlChange {
            channel,
            controller,
            value,
        };
        let rpn = |out: &mut MidiBuffer, ch: u8, n: u8, value: u8| {
            for ev in [
                cc(ch, 101, 0),
                cc(ch, 100, n),
                cc(ch, 6, value),
                cc(ch, 38, 0),
                cc(ch, 101, 127),
                cc(ch, 100, 127),
            ] {
                let _ = out.push(TimedMidiEvent::new(0, ev));
            }
        };
        let members = cfg.members.clamp(1, 15);
        rpn(out, 0, 6, members);
        for ch in 1..=members {
            rpn(out, ch, 0, cfg.bend_range.min(127));
        }
    }

    /// Send the articulation state in effect at lane time `pos` at block
    /// offset `at`: its controllers, program and keyswitch note-ons, the
    /// note-offs a sample later (in the next block when this one ends
    /// first). Realtime-safe.
    fn chase_switch(
        &mut self,
        art: &ArticulationLane,
        pos: i64,
        at: u32,
        frames: usize,
        out: &mut MidiBuffer,
    ) {
        let Some(state) = art.at(pos).and_then(|s| art.state(s)) else {
            return;
        };
        for &ev in state.on.iter() {
            if out.push(TimedMidiEvent::new(at, ev)).is_ok() {
                self.tracker.observe(ev);
                if let Some((ch, i)) = slot(ev) {
                    self.sent[ch][i] = Some((ev, Fine::NONE));
                }
            }
        }
        for &ev in state.off.iter() {
            if (at as usize) + 1 < frames {
                if out.push(TimedMidiEvent::new(at + 1, ev)).is_ok() {
                    self.tracker.observe(ev);
                }
            } else if let Some(free) = self.pending_offs.iter_mut().find(|p| p.is_none()) {
                *free = Some(ev);
            }
        }
    }

    /// Send the controller values in effect at `pos` (realtime-safe).
    fn chase_to(&mut self, cx: &EngineContext, pos: i64, frames: usize, out: &mut MidiBuffer) {
        let Some(lane) = cx.timeline.lane(self.track) else {
            return;
        };
        for slots in self.chase.iter_mut() {
            slots.fill(None);
        }
        let upto = lane.midi.partition_point(|r| r.start <= pos);
        for region in lane.midi[..upto].iter().filter(|r| r.end > pos) {
            // From the chase point before `pos` (not the region's start).
            let to = region.events.partition_point(|(time, _, _)| *time < pos);
            let chase = &mut self.chase;
            let from = region.chase.load(to, |key, ev, fine| {
                let key = usize::from(key);
                chase[key / SLOTS][key % SLOTS] = Some((ev, fine));
            });
            for &(_, ev, fine) in &region.events[from..to] {
                if let Some((ch, i)) = slot(ev) {
                    self.chase[ch][i] = Some((ev, fine));
                }
            }
        }
        for ch in 0..16 {
            for i in 0..SLOTS {
                if let Some((ev, fine)) = self.chase[ch][i]
                    && self.sent[ch][i] != Some((ev, fine))
                    && out.push(TimedMidiEvent::with_fine(0, ev, fine)).is_ok()
                {
                    self.sent[ch][i] = Some((ev, fine));
                }
            }
        }
        // The articulation in effect (after the controllers: a switch's
        // controller wins).
        if let Some(art) = lane.articulation.as_deref() {
            self.chase_switch(art, pos, 0, frames, out);
        }
    }
}

impl MidiClipPlayer {
    /// Emit `lane`'s events in lane time `[pos, end)`, lane time `t` at
    /// block offset `t - shift`.
    fn play_span(&mut self, lane: &Lane, out: &mut MidiBuffer, pos: i64, end: i64, shift: i64) {
        // Articulation switches before the notes: their note-ons and
        // controllers a sample ahead, their keyswitches' note-offs on the
        // note's sample (sorted before its note-on).
        if let Some(art) = lane.articulation.as_deref() {
            let first = art.switches.partition_point(|s| s.at - SWITCH_LEAD < pos);
            for s in &art.switches[first..] {
                let t = s.at - SWITCH_LEAD;
                if t >= end {
                    break;
                }
                let Some(state) = art.state(s) else { continue };
                for &ev in state.on.iter() {
                    if out
                        .push(TimedMidiEvent::new((t - shift) as u32, ev))
                        .is_ok()
                    {
                        self.tracker.observe(ev);
                        if let Some((ch, i)) = slot(ev) {
                            self.sent[ch][i] = Some((ev, Fine::NONE));
                        }
                    }
                }
            }
            let first = art.switches.partition_point(|s| s.at < pos);
            for s in &art.switches[first..] {
                if s.at >= end {
                    break;
                }
                let Some(state) = art.state(s) else { continue };
                for &ev in state.off.iter() {
                    if out
                        .push(TimedMidiEvent::new((s.at - shift) as u32, ev))
                        .is_ok()
                    {
                        self.tracker.observe(ev);
                    }
                }
            }
        }
        let upto = lane.midi.partition_point(|r| r.start < end);
        for region in lane.midi[..upto].iter().filter(|r| r.end >= pos) {
            // SysEx for the track's plugins (bytes copied into the buffer).
            let first = region.sysex.partition_point(|(time, _)| *time < pos);
            for (time, bytes) in &region.sysex[first..] {
                if *time >= end {
                    break;
                }
                let _ = out.push_sysex((*time - shift) as u32, bytes);
            }
            let first = region.events.partition_point(|(time, _, _)| *time < pos);
            for &(time, event, fine) in &region.events[first..] {
                if time >= end {
                    break;
                }
                if out
                    .push(TimedMidiEvent::with_fine(
                        (time - shift) as u32,
                        event,
                        fine,
                    ))
                    .is_ok()
                {
                    self.tracker.observe(event);
                    if let Some((ch, i)) = slot(event) {
                        self.sent[ch][i] = Some((event, fine));
                    }
                }
            }
        }
    }
}

impl Processor<EngineContext> for MidiClipPlayer {
    fn process(&mut self, cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        let Some(out) = io.events_out.first_mut() else {
            return;
        };
        let t = &cx.data.transport;
        // Keyswitch note-offs a chase left for this block.
        for p in self.pending_offs.iter_mut() {
            if let Some(ev) = p.take()
                && out.push(TimedMidiEvent::new(0, ev)).is_ok()
            {
                self.tracker.observe(ev);
            }
        }
        let muted = self.mute.is_some_and(|m| cx.data.params.get(m) >= 0.5);
        if cx.data.discontinuity || (self.was_playing && !t.playing) || (muted && !self.muted) {
            self.tracker.release_all(out, 0);
            self.rest(out);
        }
        // Unmuted mid-clip: the controllers in effect, as when starting.
        let unmuted = self.muted && !muted;
        self.muted = muted;
        if muted {
            self.was_playing = t.playing;
            return;
        }
        let started = t.playing && (!self.was_playing || cx.data.discontinuity || unmuted);
        self.was_playing = t.playing;
        if !t.playing {
            return;
        }
        let pos = t.sample_position;
        let timeline = &cx.data.timeline;
        let (pieces, count) = launch::pieces(cx.data.launch.track(self.track), pos, io.frames);
        if started {
            if let Some(cfg) = timeline.lane(self.track).and_then(|l| l.mpe) {
                Self::announce_mpe(cfg, out);
            }
            if pieces[0].2 == Play::Arrangement {
                self.chase_to(cx.data, pos, io.frames, out);
            }
            self.playing = pieces[0].2;
        }
        // The arrangement, or a launched clip looping from its start; notes
        // sounding where one gives way to another (or the clip wraps) end
        // there.
        for &(o, n, play) in &pieces[..count] {
            if play != self.playing {
                self.tracker.release_all(out, o as u32);
                self.playing = play;
            }
            let from = pos + o as i64;
            let to = from + n as i64;
            match play {
                Play::Arrangement => {
                    if let Some(lane) = timeline.lane(self.track) {
                        self.play_span(lane, out, from, to, pos);
                    }
                }
                Play::Clip { slot, start } => {
                    let Some(l) = timeline.launch_lane(slot) else {
                        continue;
                    };
                    let mut t = from;
                    while t < to {
                        let local = (t - start).rem_euclid(l.length);
                        if local == 0 && t != start {
                            self.tracker.release_all(out, (t - pos) as u32);
                        }
                        // From the clip's start (each time round): the
                        // articulation its first note wants.
                        if local == 0
                            && let Some(art) = l.lane.articulation.as_deref()
                        {
                            self.chase_switch(art, 0, (t - pos) as u32, io.frames, out);
                        }
                        let m = (l.length - local).min(to - t);
                        self.play_span(&l.lane, out, local, local + m, local - (t - pos));
                        t += m;
                    }
                }
                Play::Silence => {}
            }
        }
    }

    fn reset(&mut self) {
        self.tracker = NoteTracker::default();
        for slots in self.sent.iter_mut() {
            slots.fill(None);
        }
        self.pending_offs = [None; PENDING_OFFS];
        self.was_playing = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{CHASE_EVERY, ChasePoints};

    /// Starting anywhere, the values from the chase point before and the
    /// events after it are the values a scan from the region's start finds.
    #[test]
    fn chase_points_give_what_a_scan_from_the_start_gives() {
        let mut events = Vec::new();
        let mut t = 0i64;
        for i in 0..(CHASE_EVERY * 7 + 123) {
            t += (i % 3) as i64;
            let ev = match i % 5 {
                0 => MidiEvent::ControlChange {
                    channel: (i % 2) as u8,
                    controller: 1,
                    value: (i % 128) as u8,
                },
                1 => MidiEvent::NoteOn {
                    channel: 0,
                    key: 60,
                    velocity: 100,
                },
                2 => MidiEvent::PitchBend {
                    channel: 3,
                    value: (i * 7 % 16384) as u16,
                },
                // A controller set once, early: points far on still know it.
                3 if i == 3 => MidiEvent::ControlChange {
                    channel: 9,
                    controller: 64,
                    value: 127,
                },
                3 => MidiEvent::ProgramChange {
                    channel: 1,
                    program: (i % 128) as u8,
                },
                _ => MidiEvent::NoteOff {
                    channel: 0,
                    key: 60,
                    velocity: 0,
                },
            };
            // Every other controller value with its 32 bits.
            let fine = match ev {
                MidiEvent::ControlChange { value, .. } if i % 2 == 0 => {
                    Fine::value(u32::from(value) << 25 | i as u32)
                }
                _ => Fine::NONE,
            };
            events.push((t, ev, fine));
        }
        let points = ChasePoints::new(&events, chase_key);
        for upto in (0..=events.len()).step_by(37).chain([events.len()]) {
            let mut scanned = std::collections::HashMap::new();
            for &(_, ev, fine) in &events[..upto] {
                if let Some(k) = chase_key(ev) {
                    scanned.insert(k, (ev, fine));
                }
            }
            let mut chased = std::collections::HashMap::new();
            let from = points.load(upto, |k, ev, fine| {
                chased.insert(k, (ev, fine));
            });
            assert!(
                from <= upto && upto - from <= CHASE_EVERY,
                "{from} for {upto}"
            );
            for &(_, ev, fine) in &events[from..upto] {
                if let Some(k) = chase_key(ev) {
                    chased.insert(k, (ev, fine));
                }
            }
            assert_eq!(chased, scanned, "up to event {upto}");
        }
    }
}
