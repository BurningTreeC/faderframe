//! Pitch Correct's presets: from a correction nobody hears (slow, the
//! vibrato and the scoops kept) to the hard-tuned sound (at once, flat
//! notes), and the voice moved by octaves with or without its formants.

use super::{FactoryPreset, preset};

pub(super) fn pitch_correct() -> Vec<FactoryPreset> {
    use crate::devices::pitch_correct::id::*;
    use crate::devices::pitch_correct::target;
    let to = |t: usize| (TARGET, t as f64);
    vec![
        preset(
            "Natural Lead",
            &[(SPEED, 60.0), (HUMANIZE, 0.4), (VIBRATO, 1.0)],
        ),
        preset(
            "Gentle Polish",
            &[(SPEED, 140.0), (HUMANIZE, 0.6), (VIBRATO, 1.0)],
        ),
        preset(
            "Tight Pop Vocal",
            &[(SPEED, 20.0), (HUMANIZE, 0.25), (VIBRATO, 0.8)],
        ),
        preset(
            "Hard Tune",
            &[(SPEED, 0.0), (HUMANIZE, 0.0), (VIBRATO, 0.0)],
        ),
        preset(
            "Robot",
            &[to(target::CHROMATIC), (SPEED, 0.0), (VIBRATO, 0.0)],
        ),
        preset(
            "Backing Vocals",
            &[(SPEED, 30.0), (HUMANIZE, 0.3), (VIBRATO, 0.5)],
        ),
        preset(
            "Chord Tones",
            &[to(target::CHORD_TRACK), (SPEED, 40.0), (VIBRATO, 0.8)],
        ),
        preset(
            "Chromatic Safety Net",
            &[
                to(target::CHROMATIC),
                (SPEED, 90.0),
                (HUMANIZE, 0.5),
                (VIBRATO, 1.0),
            ],
        ),
        preset(
            "Follow a MIDI Melody",
            &[to(target::MIDI), (SPEED, 10.0), (VIBRATO, 0.4)],
        ),
        preset(
            "Wide Vibrato",
            &[(SPEED, 30.0), (VIBRATO, 1.7), (HUMANIZE, 0.2)],
        ),
        preset(
            "Octave Down",
            &[(TRANSPOSE, -12.0), (SPEED, 40.0), (VIBRATO, 1.0)],
        ),
        preset(
            "Octave Up",
            &[(TRANSPOSE, 12.0), (SPEED, 40.0), (VIBRATO, 1.0)],
        ),
        preset(
            "Chipmunk",
            &[(TRANSPOSE, 7.0), (PRESERVE_FORMANTS, 0.0), (SPEED, 30.0)],
        ),
        preset(
            "Giant",
            &[(TRANSPOSE, -5.0), (FORMANT, -6.0), (SPEED, 30.0)],
        ),
        preset(
            "Low Male Voice",
            &[(RANGE, 2.0), (SPEED, 50.0), (HUMANIZE, 0.4)],
        ),
    ]
}
