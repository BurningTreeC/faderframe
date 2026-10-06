use crate::context::EngineContext;
use faderframe_audio_graph::{NodeIo, ProcessContext, Processor};
use faderframe_core::TrackId;
use faderframe_midi::{MidiBuffer, MidiEvent, NoteTracker, TimedMidiEvent};
use faderframe_realtime::ParamSlot;

/// Controller slots per channel: 128 CCs, pitch bend, channel pressure.
const SLOTS: usize = 130;
const PB: usize = 128;
const AT: usize = 129;

fn slot(ev: MidiEvent) -> Option<(usize, usize)> {
    match ev {
        MidiEvent::ControlChange {
            channel,
            controller,
            ..
        } => Some(((channel & 15) as usize, (controller & 127) as usize)),
        MidiEvent::PitchBend { channel, .. } => Some(((channel & 15) as usize, PB)),
        MidiEvent::ChannelPressure { channel, .. } => Some(((channel & 15) as usize, AT)),
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
/// wheel), and controllers the player moved go back to rest (sustain off,
/// bend centred) when playback jumps or stops. All state is fixed-size.
pub struct MidiClipPlayer {
    track: TrackId,
    /// A MIDI track's mute: while set, it plays nothing (its sounding
    /// notes are released, controllers chased again when it comes back).
    mute: Option<ParamSlot>,
    muted: bool,
    tracker: NoteTracker,
    /// Value last sent per channel and controller slot.
    sent: Box<[[Option<MidiEvent>; SLOTS]; 16]>,
    /// Scratch for chasing.
    chase: Box<[[Option<MidiEvent>; SLOTS]; 16]>,
    was_playing: bool,
}

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
                let Some(ev) = s.take() else { continue };
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

    /// Send the controller values in effect at `pos` (realtime-safe).
    fn chase_to(&mut self, cx: &EngineContext, pos: i64, out: &mut MidiBuffer) {
        let Some(lane) = cx.timeline.lane(self.track) else {
            return;
        };
        for slots in self.chase.iter_mut() {
            slots.fill(None);
        }
        let upto = lane.midi.partition_point(|r| r.start <= pos);
        for region in lane.midi[..upto].iter().filter(|r| r.end > pos) {
            for &(time, ev) in &region.events {
                if time >= pos {
                    break;
                }
                if let Some((ch, i)) = slot(ev) {
                    self.chase[ch][i] = Some(ev);
                }
            }
        }
        for ch in 0..16 {
            for i in 0..SLOTS {
                if let Some(ev) = self.chase[ch][i]
                    && self.sent[ch][i] != Some(ev)
                    && out.push(TimedMidiEvent::new(0, ev)).is_ok()
                {
                    self.sent[ch][i] = Some(ev);
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
        if started {
            if let Some(cfg) = cx.data.timeline.lane(self.track).and_then(|l| l.mpe) {
                Self::announce_mpe(cfg, out);
            }
            self.chase_to(cx.data, pos, out);
        }
        let Some(lane) = cx.data.timeline.lane(self.track) else {
            return;
        };
        let end = pos + io.frames as i64;
        let upto = lane.midi.partition_point(|r| r.start < end);
        for region in lane.midi[..upto].iter().filter(|r| r.end >= pos) {
            // SysEx for the track's plugins (bytes copied into the buffer).
            let first = region.sysex.partition_point(|(time, _)| *time < pos);
            for (time, bytes) in &region.sysex[first..] {
                if *time >= end {
                    break;
                }
                let _ = out.push_sysex((*time - pos) as u32, bytes);
            }
            let first = region.events.partition_point(|(time, _)| *time < pos);
            for &(time, event) in &region.events[first..] {
                if time >= end {
                    break;
                }
                if out
                    .push(TimedMidiEvent::new((time - pos) as u32, event))
                    .is_ok()
                {
                    self.tracker.observe(event);
                    if let Some((ch, i)) = slot(event) {
                        self.sent[ch][i] = Some(event);
                    }
                }
            }
        }
    }

    fn reset(&mut self) {
        self.tracker = NoteTracker::default();
        for slots in self.sent.iter_mut() {
            slots.fill(None);
        }
        self.was_playing = false;
    }
}
