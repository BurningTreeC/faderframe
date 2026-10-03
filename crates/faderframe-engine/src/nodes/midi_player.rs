use crate::context::EngineContext;
use faderframe_audio_graph::{NodeIo, ProcessContext, Processor};
use faderframe_core::TrackId;
use faderframe_midi::{NoteTracker, TimedMidiEvent};

/// Emits the MIDI events of one track's MIDI regions with exact sample
/// offsets. On every transport discontinuity (stop, locate, loop wrap) the
/// notes it started are released at offset 0, so nothing hangs.
pub struct MidiClipPlayer {
    track: TrackId,
    tracker: NoteTracker,
}

impl MidiClipPlayer {
    pub fn new(track: TrackId) -> Self {
        Self {
            track,
            tracker: NoteTracker::default(),
        }
    }
}

impl Processor<EngineContext> for MidiClipPlayer {
    fn process(&mut self, cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        let Some(out) = io.events_out.first_mut() else {
            return;
        };
        if cx.data.discontinuity {
            self.tracker.release_all(out, 0);
        }
        let t = &cx.data.transport;
        if !t.playing {
            return;
        }
        let Some(lane) = cx.data.timeline.lane(self.track) else {
            return;
        };
        let pos = t.sample_position;
        let end = pos + io.frames as i64;
        let upto = lane.midi.partition_point(|r| r.start < end);
        for region in lane.midi[..upto].iter().filter(|r| r.end >= pos) {
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
                }
            }
        }
    }

    fn reset(&mut self) {
        self.tracker = NoteTracker::default();
    }
}
