//! Singing into MIDI on the audio thread (the session's voice ports).
//!
//! The tracker ([`faderframe_analysis::voice`]) listens to one input
//! channel of the device inside the callback and its notes join the live
//! MIDI of that very callback, on the voice port and at the frame each was
//! decided, so the instrument plays them at once: only the input and output
//! buffers and the few milliseconds the pitch needs to be heard stand
//! between the voice and the sound. Each event also keeps how much earlier
//! its sound began (the block's `early`): a recording puts it there. A copy
//! of each goes to the control side (Capture MIDI, activity), stamped with
//! when it was sung.

use crate::midi::MidiInputState;
use faderframe_analysis::voice::{VoiceConfig, VoiceEvent, VoiceTracker};
use faderframe_midi::{ExpressionValue, MidiEvent, MidiInputEvent, NoteExpressionKind};

/// How sung pitch moves between notes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Glide {
    /// Steady notes.
    #[default]
    Off,
    /// The note's own tuning (note expression).
    Expression,
    /// Channel pitch bend, ±2 semitones.
    PitchBend,
}

/// The listening: a tracker, the channel it hears, the port its notes come
/// from.
pub struct VoiceRun {
    tracker: VoiceTracker,
    channel: u16,
    port: u16,
    glide: Glide,
    /// The device rate the tracker was made for (it waits for it).
    rate: u32,
    feed: rtrb::Producer<MidiInputEvent>,
}

impl VoiceRun {
    /// A listener on input `channel` (0-based) for a device at `rate`,
    /// sending as `port`; the control side's copies come out of the
    /// returned consumer.
    pub fn new(
        channel: u16,
        port: u16,
        rate: u32,
        config: VoiceConfig,
        glide: Glide,
    ) -> (Self, rtrb::Consumer<MidiInputEvent>) {
        let (feed, rx) = rtrb::RingBuffer::new(1024);
        (
            Self {
                tracker: VoiceTracker::new(f64::from(rate), config),
                channel,
                port,
                glide,
                rate,
                feed,
            },
            rx,
        )
    }

    pub(crate) fn set(&mut self, config: VoiceConfig, glide: Glide) {
        self.tracker.set_config(config);
        self.glide = glide;
    }

    pub(crate) fn channel(&self) -> usize {
        usize::from(self.channel)
    }

    pub(crate) fn rate(&self) -> u32 {
        self.rate
    }

    /// Listen to a callback's input (audio thread; allocation-free).
    /// `callback_ns`: the MIDI clock at the callback (its input arrived).
    pub(crate) fn listen(
        &mut self,
        input: &[f32],
        midi: &mut MidiInputState,
        callback_ns: u64,
        dropped: &std::sync::atomic::AtomicU64,
    ) {
        let frames = input.len();
        if frames == 0 {
            return;
        }
        let ns_per_frame = 1e9 / f64::from(self.rate.max(1));
        let (port, glide) = (self.port, self.glide);
        let feed = &mut self.feed;
        let mut sounding = self.tracker.sounding();
        let mut put = |now: isize, at: isize, e: MidiEvent| {
            let offset = now.clamp(0, frames as isize - 1) as u32;
            let early = (now - at).max(0) as u32;
            if !midi.inject(port, offset, early, e) {
                dropped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            // The sound began `frames - at` before the callback.
            let back = (frames as isize - at).max(0) as f64 * ns_per_frame;
            let time = (callback_ns as f64 - back).max(0.0) as u64;
            let copy = match e {
                MidiEvent::NoteExpression {
                    channel,
                    key,
                    kind,
                    value,
                } => Some(MidiInputEvent::expression(
                    port,
                    time,
                    channel,
                    key,
                    kind,
                    value.get(),
                )),
                e => {
                    let (bytes, len) = e.to_bytes();
                    MidiInputEvent::new(port, time, &bytes[..len])
                }
            };
            if let Some(c) = copy {
                let _ = feed.push(c);
            }
        };
        self.tracker.process(input, |now, at, e| match e {
            VoiceEvent::NoteOn { key, velocity } => {
                if glide == Glide::PitchBend {
                    put(
                        now,
                        at,
                        MidiEvent::PitchBend {
                            channel: 0,
                            value: MidiEvent::PITCH_BEND_CENTRE,
                        },
                    );
                }
                put(
                    now,
                    at,
                    MidiEvent::NoteOn {
                        channel: 0,
                        key,
                        velocity: velocity.clamp(1, 127),
                    },
                );
                sounding = Some(key);
            }
            VoiceEvent::NoteOff { key } => {
                put(
                    now,
                    at,
                    MidiEvent::NoteOff {
                        channel: 0,
                        key,
                        velocity: 0,
                    },
                );
                sounding = None;
            }
            VoiceEvent::Bend { semitones } => match (glide, sounding) {
                (Glide::Expression, Some(key)) => put(
                    now,
                    at,
                    MidiEvent::NoteExpression {
                        channel: 0,
                        key,
                        kind: NoteExpressionKind::Tuning,
                        value: ExpressionValue::new(f64::from(semitones)),
                    },
                ),
                (Glide::PitchBend, Some(_)) => {
                    let v = (f64::from(MidiEvent::PITCH_BEND_CENTRE)
                        + f64::from(semitones) / 2.0 * 8192.0)
                        .clamp(0.0, 16383.0) as u16;
                    put(
                        now,
                        at,
                        MidiEvent::PitchBend {
                            channel: 0,
                            value: v,
                        },
                    );
                }
                _ => {}
            },
        });
    }

    /// End the sounding note (the listening stops or the input went away).
    pub(crate) fn release(&mut self, midi: &mut MidiInputState) {
        if let Some(VoiceEvent::NoteOff { key }) = self.tracker.reset() {
            midi.inject(
                self.port,
                0,
                0,
                MidiEvent::NoteOff {
                    channel: 0,
                    key,
                    velocity: 0,
                },
            );
        }
    }
}
