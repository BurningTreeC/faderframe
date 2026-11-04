//! Fades of an interactive score's transitions, as its preview plays them
//! (`faderframe_session::adaptive`): the music fades out before a sync
//! point and the destination fades in after it, on the device outputs.
//!
//! A transition happens on a processing frame — a planned jump the
//! transport takes ([`faderframe_transport::Cue`]), or a timeline position
//! playback runs through (a launcher scene launched there) — and is heard
//! the graph's latency later. [`Fading`] predicts that frame while
//! playback runs straight towards the position, fixes it once the jump was
//! taken (or the position passed), and shapes the frames heard around it:
//! linear gain down over the fade-out to the frame, up over the fade-in
//! from it. A planned jump that is not taken (the transport went
//! elsewhere) cancels the fade. Realtime-safe: plain arithmetic.

use faderframe_audio::DeviceBuffers;
use faderframe_transport::TransportState;

/// A transition's fades (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransitionFade {
    /// The timeline sample the transition happens on (the sync point).
    pub at: i64,
    /// Frames heard fading out before it and in after it.
    pub out_frames: u32,
    pub in_frames: u32,
    /// The transport takes a planned jump there (else playback runs
    /// through it).
    pub cue: bool,
}

/// A transition fade under way on the audio thread.
#[derive(Debug, Default)]
pub(crate) struct Fading {
    fade: Option<TransitionFade>,
    /// The processing frame of the transition: predicted, or fixed.
    frame: Option<i64>,
    fixed: bool,
    /// The transport's cues taken when the fade came.
    taken: u32,
}

impl Fading {
    /// A new fade (`None`: none), with the transport as it is.
    pub(crate) fn set(&mut self, fade: Option<TransitionFade>, transport: &TransportState) {
        self.fade = fade;
        self.frame = None;
        self.fixed = false;
        self.taken = transport.cues_taken();
    }

    pub(crate) fn active(&self) -> bool {
        self.fade.is_some()
    }

    /// Before a chunk starting on processing frame `frame`: where the
    /// transition will be, while playback runs straight to it.
    pub(crate) fn before_chunk(&mut self, frame: i64, transport: &TransportState) {
        let Some(f) = self.fade else { return };
        if self.fixed {
            return;
        }
        let pos = transport.position();
        if !transport.playing() || transport.scrubbing() || transport.counting_in() > 0 {
            self.frame = None;
            return;
        }
        // A loop end before the position wraps first: not straight.
        let wraps_first = transport.looping()
            && transport
                .loop_range()
                .is_some_and(|r| pos < r.end && r.end < f.at);
        if pos <= f.at && !wraps_first {
            self.frame = Some(frame + (f.at - pos));
        } else if f.cue {
            // Past it without the jump (or going round first): not now.
            self.frame = None;
        }
    }

    /// After the chunk `[frame, end)` advanced the transport: the jump
    /// taken in it (on its last frame) or the position passed.
    pub(crate) fn after_chunk(&mut self, end: i64, transport: &TransportState) {
        let Some(f) = self.fade else { return };
        if self.fixed {
            return;
        }
        if f.cue {
            if transport.cues_taken() != self.taken {
                self.frame = Some(end);
                self.fixed = true;
            } else if self.frame.is_some_and(|fr| fr < end) {
                // Its frame went by and the jump did not come.
                self.fade = None;
                self.frame = None;
            }
        } else if self.frame.is_some_and(|fr| fr <= end) {
            self.fixed = true;
        }
    }

    /// The gain for output frame `frame` with the graph's `latency`.
    fn gain(&self, frame: i64, latency: i64) -> f32 {
        let (Some(f), Some(at)) = (self.fade, self.frame) else {
            return 1.0;
        };
        let d = frame - latency - at;
        if d < 0 {
            let out = i64::from(f.out_frames);
            if out > 0 && -d <= out {
                return (-d) as f32 / out as f32;
            }
            1.0
        } else {
            let fade_in = i64::from(f.in_frames);
            if fade_in > 0 && d < fade_in {
                return d as f32 / fade_in as f32;
            }
            1.0
        }
    }

    /// Shape the output frames `[offset, offset + n)` of `io`, the first of
    /// them frame `frame`; the fade is over once they are past it.
    pub(crate) fn apply(
        &mut self,
        io: &mut dyn DeviceBuffers,
        offset: usize,
        n: usize,
        frame: i64,
        latency: u32,
    ) {
        let (Some(f), Some(at)) = (self.fade, self.frame) else {
            return;
        };
        let latency = i64::from(latency);
        let first = frame - latency - at;
        let last = first + n as i64;
        let out = i64::from(f.out_frames);
        let fade_in = i64::from(f.in_frames);
        if last <= -out || (self.fixed && first >= fade_in) {
            if self.fixed && first >= fade_in {
                self.fade = None;
                self.frame = None;
            }
            return;
        }
        for c in 0..io.output_channels() {
            let dst = &mut io.output(c)[offset..offset + n];
            for (i, o) in dst.iter_mut().enumerate() {
                *o *= self.gain(frame + i as i64, latency);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_transport::{Cue, Cues, TransportCommand};

    #[test]
    fn a_jump_fades_out_to_its_frame_and_in_after_it_as_heard() {
        let mut t = TransportState::default();
        t.apply(TransportCommand::Play);
        t.apply(TransportCommand::SetCues(Cues::new(&[Cue {
            at: 1_000,
            to: 50_000,
            then_loop: None,
        }])));
        let mut f = Fading::default();
        f.set(
            Some(TransitionFade {
                at: 1_000,
                out_frames: 100,
                in_frames: 200,
                cue: true,
            }),
            &t,
        );
        // Frame counter 10 000 at position 0: the jump on frame 11 000.
        f.before_chunk(10_000, &t);
        assert_eq!(f.frame, Some(11_000));
        // Heard 64 frames later.
        assert_eq!(f.gain(11_064 - 100, 64), 1.0);
        assert_eq!(f.gain(11_064 - 50, 64), 0.5);
        assert_eq!(f.gain(11_064, 64), 0.0);
        assert_eq!(f.gain(11_064 + 100, 64), 0.5);
        assert_eq!(f.gain(11_064 + 200, 64), 1.0);
        // The jump taken at the end of the chunk that reached it.
        let n = t.frames_until_wrap(4096);
        assert_eq!(n, 1_000);
        t.advance(n);
        f.after_chunk(11_000, &t);
        assert!(f.fixed);
        assert_eq!(f.frame, Some(11_000));
        // Once past the fade-in, it is over.
        struct Out(Vec<Vec<f32>>);
        impl DeviceBuffers for Out {
            fn frames(&self) -> usize {
                self.0[0].len()
            }
            fn input_channels(&self) -> usize {
                0
            }
            fn output_channels(&self) -> usize {
                self.0.len()
            }
            fn input(&self, _: usize) -> &[f32] {
                &[]
            }
            fn output(&mut self, c: usize) -> &mut [f32] {
                &mut self.0[c]
            }
        }
        let mut io = Out(vec![vec![1.0; 64]; 2]);
        f.apply(&mut io, 0, 64, 11_064 + 100, 64);
        assert!((io.0[1][0] - 0.5).abs() < 1e-6);
        assert!(f.active());
        let mut io = Out(vec![vec![1.0; 64]; 2]);
        f.apply(&mut io, 0, 64, 11_064 + 300, 64);
        assert_eq!(io.0[0][0], 1.0);
        assert!(!f.active());
    }

    #[test]
    fn a_jump_that_does_not_come_cancels_its_fade() {
        let mut t = TransportState::default();
        t.apply(TransportCommand::Play);
        let mut f = Fading::default();
        f.set(
            Some(TransitionFade {
                at: 500,
                out_frames: 100,
                in_frames: 100,
                cue: true,
            }),
            &t,
        );
        f.before_chunk(0, &t);
        assert_eq!(f.frame, Some(500));
        t.advance(1_000);
        f.after_chunk(1_000, &t);
        assert!(!f.active());
    }
}
