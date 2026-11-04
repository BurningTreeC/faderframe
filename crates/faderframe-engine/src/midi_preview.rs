//! A MIDI file previewed from the media browser: its events played into
//! the instrument the editor auditions (the selected instrument track,
//! [`crate::midi::MidiShared::audition_track`]) through the audition port,
//! as a key the piano roll plays is — whatever the track's live state, and
//! never recorded.
//!
//! The control side turns the file into frames (at the project's tempo
//! when it plays in step, else the file's own) and hands a
//! [`MidiPreview`] over ([`crate::EngineController::set_midi_preview`]);
//! the audio thread puts the events due in each callback into its live
//! MIDI at their offsets. [`PreviewStart::Now`] plays at once on its own;
//! [`PreviewStart::At`] starts when the transport reaches a timeline frame
//! and follows it (a loop stays in phase with the bars; a locate or wrap
//! ends the notes sounding and goes on from the new place; a stop ends
//! it). Whatever sounds when a preview ends or is replaced is released.
//! Realtime: no allocation (the events are read in place; the runs come
//! and go through the message queue and the garbage queue).

use crate::midi::{AUDITION_PORT, MidiInputState};
use crate::nodes::PreviewStart;
use faderframe_midi::MidiEvent;
use faderframe_transport::TransportState;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

/// A MIDI file as frames.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MidiPreview {
    /// (frame from the start, the message's bytes), in order.
    pub events: Vec<(i64, [u8; 3])>,
    /// The length (frames): a loop's, or where a one-shot ends.
    pub length: i64,
    pub looped: bool,
    pub start: Option<PreviewStart>,
}

/// Where a MIDI preview is (shared with the control side).
#[derive(Debug, Default)]
pub struct MidiPreviewStatus {
    position: AtomicI64,
    ended: AtomicBool,
}

impl MidiPreviewStatus {
    /// The frame of the file played last (`None`: not started).
    pub fn position(&self) -> Option<i64> {
        let p = self.position.load(Ordering::Relaxed);
        (p >= 0).then_some(p)
    }

    /// Played out or stopped.
    pub fn has_ended(&self) -> bool {
        self.ended.load(Ordering::Relaxed)
    }
}

/// The audio thread's run of a [`MidiPreview`].
pub struct MidiPreviewRun {
    preview: MidiPreview,
    status: Arc<MidiPreviewStatus>,
    /// The next event to play.
    next: usize,
    /// Where the file is (the frame after the last one played; `Now`: its
    /// own clock).
    at: i64,
    started: bool,
    ended: bool,
    /// Keys sounding, a bit per key and channel.
    held: [u128; 16],
}

impl MidiPreviewRun {
    /// Control side: a run of `preview` and its status.
    pub fn new(mut preview: MidiPreview) -> (Self, Arc<MidiPreviewStatus>) {
        preview.events.sort_by_key(|e| e.0);
        if preview.length <= 0 {
            preview.length = preview.events.last().map_or(0, |e| e.0 + 1);
        }
        let status = Arc::new(MidiPreviewStatus {
            position: AtomicI64::new(-1),
            ended: AtomicBool::new(false),
        });
        (
            Self {
                preview,
                status: Arc::clone(&status),
                next: 0,
                at: 0,
                started: false,
                ended: false,
                held: [0; 16],
            },
            status,
        )
    }

    /// Release every key still sounding: now (`offset` in the callback) or,
    /// when the callback's MIDI is not taken yet, with the next one.
    pub(crate) fn release(&mut self, midi: &mut MidiInputState, offset: Option<u32>) {
        for (ch, keys) in self.held.iter_mut().enumerate() {
            let mut k = *keys;
            while k != 0 {
                let key = k.trailing_zeros() as u8;
                k &= k - 1;
                let off = MidiEvent::NoteOff {
                    channel: ch as u8,
                    key,
                    velocity: 0,
                };
                match offset {
                    Some(o) => {
                        let _ = midi.inject(AUDITION_PORT, o, 0, off);
                    }
                    None => midi.defer(AUDITION_PORT, off),
                }
            }
            *keys = 0;
        }
    }

    fn end(&mut self, midi: &mut MidiInputState, offset: u32) {
        if !self.ended {
            self.release(midi, Some(offset));
            self.ended = true;
            self.status.ended.store(true, Ordering::Relaxed);
        }
    }

    /// The events of file frames `[from, to)` at callback offsets from
    /// `offset` (file frame `from` at `offset`).
    fn play_span(&mut self, midi: &mut MidiInputState, from: i64, to: i64, offset: usize) {
        if from != self.at {
            // Jumped: what sounds ends, and the file goes on from there.
            self.release(midi, Some(offset as u32));
            self.next = self.preview.events.partition_point(|e| e.0 < from);
        }
        while let Some(&(f, bytes)) = self.preview.events.get(self.next) {
            if f >= to {
                break;
            }
            self.next += 1;
            if f < from {
                continue;
            }
            let Some(ev) = MidiEvent::from_bytes(&bytes) else {
                continue;
            };
            let o = (offset as i64 + f - from) as u32;
            match ev {
                MidiEvent::NoteOn {
                    channel,
                    key,
                    velocity,
                } if velocity > 0 => {
                    self.held[usize::from(channel & 15)] |= 1u128 << (key & 127);
                }
                MidiEvent::NoteOn { channel, key, .. }
                | MidiEvent::NoteOff { channel, key, .. } => {
                    self.held[usize::from(channel & 15)] &= !(1u128 << (key & 127));
                }
                _ => {}
            }
            let _ = midi.inject(AUDITION_PORT, o, 0, ev);
        }
        self.at = to;
        self.status.position.store(to, Ordering::Relaxed);
    }

    /// File frames `[f, f + len)` at `offset`: wrapped at the length when
    /// looping, the end noted when not.
    fn play_frames(&mut self, midi: &mut MidiInputState, mut f: i64, len: usize, offset: usize) {
        let length = self.preview.length.max(1);
        let mut done = 0usize;
        while done < len && !self.ended {
            if self.preview.looped {
                f = f.rem_euclid(length);
                let k = ((length - f) as usize).min(len - done);
                self.play_span(midi, f, f + k as i64, offset + done);
                if f + k as i64 >= length {
                    // Round again: the notes the loop cut end at its end.
                    self.release(midi, Some((offset + done + k).saturating_sub(1) as u32));
                    self.at = 0;
                    self.next = 0;
                }
                f += k as i64;
                done += k;
            } else {
                if f >= length {
                    self.end(midi, (offset + done) as u32);
                    break;
                }
                let k = ((length - f) as usize).min(len - done);
                self.play_span(midi, f, f + k as i64, offset + done);
                f += k as i64;
                done += k;
            }
        }
    }

    /// Audio thread: this callback's events (`frames` long, after its MIDI
    /// was taken) for a transport at its start.
    pub(crate) fn play(
        &mut self,
        midi: &mut MidiInputState,
        transport: &TransportState,
        frames: usize,
    ) {
        if self.ended || frames == 0 {
            return;
        }
        match self.preview.start {
            None | Some(PreviewStart::Now) => {
                self.started = true;
                let f = self.at;
                self.play_frames(midi, f, frames, 0);
            }
            Some(PreviewStart::At(at)) => {
                if !transport.playing() || transport.counting_in() > 0 || transport.scrubbing() {
                    if self.started {
                        self.end(midi, 0);
                    }
                    return;
                }
                // The callback's timeline frames: on to a loop's end, then
                // from its start.
                let first = transport.frames_until_wrap(frames).min(frames);
                let mut runs = [(transport.position(), 0usize, first), (0, first, 0)];
                if first < frames
                    && let Some(range) = transport.loop_range()
                {
                    runs[1] = (range.start, first, frames - first);
                }
                for (pos, offset, len) in runs {
                    if len == 0 || self.ended {
                        continue;
                    }
                    let f = pos - at;
                    if !self.started {
                        if f + len as i64 <= 0 {
                            continue;
                        }
                        self.started = true;
                        self.at = f.max(0);
                        self.next = 0;
                    }
                    // Before its start (a wrap back over it): silence.
                    let skip = if f < 0 && !self.preview.looped {
                        ((-f) as usize).min(len)
                    } else {
                        0
                    };
                    if skip == len {
                        self.release(midi, Some(offset as u32));
                        continue;
                    }
                    let from = f + skip as i64;
                    self.play_frames(midi, from, len - skip, offset + skip);
                }
            }
        }
    }
}
