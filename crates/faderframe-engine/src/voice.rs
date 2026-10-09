//! Singing into MIDI on the audio thread (the session's voice ports).
//!
//! Each listener's mono or polyphonic tracker listens to one input channel
//! of the device inside the callback and its notes join the live MIDI of
//! that very callback, on its voice port, on its MIDI channel and at the
//! frame each was decided, so the instrument plays them at once: only the
//! input and output buffers and the pitch analysis stand between the voice
//! and the sound. Several listeners run side by side (one per input a
//! track takes, see [`crate::EngineController::add_voice`]). Each event
//! also keeps how much earlier its sound began (the block's `early`): a
//! recording puts it there. A copy of each goes to the control side
//! (Capture MIDI, activity), stamped with when it was sung.

use crate::midi::MidiInputState;
use faderframe_analysis::polyvoice::PolyTracker;
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

/// Listeners the engine runs at once, at most.
pub const MAX_VOICES: usize = 64;

/// The listening: a tracker, the input channel it hears, the port and the
/// MIDI channel its notes come on.
pub struct VoiceRun {
    tracker: Tracker,
    input: u16,
    port: u16,
    /// The MIDI channel (0-based) its notes are on.
    midi_channel: u8,
    glide: Glide,
    /// The device rate the tracker was made for (it waits for it).
    rate: u32,
    feed: rtrb::Producer<MidiInputEvent>,
}

enum Tracker {
    Mono(Box<VoiceTracker>),
    Poly(Box<PolyTracker>),
}

impl Tracker {
    fn set_config(&mut self, config: VoiceConfig) {
        match self {
            Self::Mono(t) => t.set_config(config),
            Self::Poly(t) => t.set_config(config),
        }
    }

    fn process(&mut self, input: &[f32], emit: impl FnMut(isize, isize, VoiceEvent)) {
        match self {
            Self::Mono(t) => t.process(input, emit),
            Self::Poly(t) => t.process(input, emit),
        }
    }

    fn reset(&mut self, mut emit: impl FnMut(VoiceEvent)) {
        match self {
            Self::Mono(t) => {
                if let Some(e) = t.reset() {
                    emit(e);
                }
            }
            Self::Poly(t) => t.reset(emit),
        }
    }
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
        Self::with_tracker(
            channel,
            port,
            rate,
            glide,
            Tracker::Mono(Box::new(VoiceTracker::new(f64::from(rate), config))),
        )
    }

    /// A polyphonic listener. Use note expression for independent glides;
    /// channel pitch bend is suppressed because it would bend the entire chord.
    pub fn new_polyphonic(
        channel: u16,
        port: u16,
        rate: u32,
        config: VoiceConfig,
        glide: Glide,
    ) -> (Self, rtrb::Consumer<MidiInputEvent>) {
        Self::with_tracker(
            channel,
            port,
            rate,
            glide,
            Tracker::Poly(Box::new(PolyTracker::new(f64::from(rate), config))),
        )
    }

    fn with_tracker(
        channel: u16,
        port: u16,
        rate: u32,
        glide: Glide,
        tracker: Tracker,
    ) -> (Self, rtrb::Consumer<MidiInputEvent>) {
        let (feed, rx) = rtrb::RingBuffer::new(1024);
        (
            Self {
                tracker,
                input: channel,
                port,
                midi_channel: 0,
                glide,
                rate,
                feed,
            },
            rx,
        )
    }

    pub(crate) fn set(&mut self, config: VoiceConfig, glide: Glide, midi: &mut MidiInputState) {
        if self.glide != glide {
            self.release(midi);
        }
        self.tracker.set_config(config);
        self.glide = glide;
    }

    /// Its notes on MIDI channel `channel` (0-based; the first by
    /// default).
    pub fn on_channel(mut self, channel: u8) -> Self {
        self.midi_channel = channel.min(15);
        self
    }

    /// The input channel it hears (0-based).
    pub fn input(&self) -> u16 {
        self.input
    }

    /// The port its notes come from.
    pub fn port(&self) -> u16 {
        self.port
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
        let (port, glide, channel) = (self.port, self.glide, self.midi_channel);
        let polyphonic = matches!(self.tracker, Tracker::Poly(_));
        let feed = &mut self.feed;
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
                if glide == Glide::PitchBend && !polyphonic {
                    put(
                        now,
                        at,
                        MidiEvent::PitchBend {
                            channel,
                            value: MidiEvent::PITCH_BEND_CENTRE,
                        },
                    );
                }
                put(
                    now,
                    at,
                    MidiEvent::NoteOn {
                        channel,
                        key,
                        velocity: velocity.clamp(1, 127),
                    },
                );
            }
            VoiceEvent::NoteOff { key } => {
                put(
                    now,
                    at,
                    MidiEvent::NoteOff {
                        channel,
                        key,
                        velocity: 0,
                    },
                );
            }
            VoiceEvent::Bend { key, semitones } => match glide {
                Glide::Expression => put(
                    now,
                    at,
                    MidiEvent::NoteExpression {
                        channel,
                        key,
                        kind: NoteExpressionKind::Tuning,
                        value: ExpressionValue::new(f64::from(semitones)),
                    },
                ),
                Glide::PitchBend if !polyphonic => {
                    let v = (f64::from(MidiEvent::PITCH_BEND_CENTRE)
                        + f64::from(semitones) / 2.0 * 8192.0)
                        .clamp(0.0, 16383.0) as u16;
                    put(now, at, MidiEvent::PitchBend { channel, value: v });
                }
                _ => {}
            },
        });
    }

    /// End every sounding note (the listening stops or the input went away).
    pub(crate) fn release(&mut self, midi: &mut MidiInputState) {
        let mut released = false;
        let channel = self.midi_channel;
        let time = midi.clock().map(|clock| clock.now_ns());
        let mut put = |event: MidiEvent| {
            midi.defer(self.port, event);
            if let Some(time) = time {
                let (bytes, len) = event.to_bytes();
                if let Some(copy) = MidiInputEvent::new(self.port, time, &bytes[..len]) {
                    let _ = self.feed.push(copy);
                }
            }
        };
        self.tracker.reset(|e| {
            if let VoiceEvent::NoteOff { key } = e {
                released = true;
                put(MidiEvent::NoteOff {
                    channel,
                    key,
                    velocity: 0,
                });
            }
        });
        if released && self.glide == Glide::PitchBend && matches!(self.tracker, Tracker::Mono(_)) {
            put(MidiEvent::PitchBend {
                channel,
                value: MidiEvent::PITCH_BEND_CENTRE,
            });
        }
    }
}
