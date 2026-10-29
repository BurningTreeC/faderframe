//! What the rules report: findings with their measured evidence, the
//! severity and confidence they were given (and from what), and the
//! experiments they suggest.
//!
//! Findings name tracks by id; titles and explanations are written when
//! they are shown ([`Finding::title`]), with the names the tracks have
//! then — a renamed track keeps its findings.

use crate::bands::format_hz;
use faderframe_core::{PluginInstanceId, ReferenceId, TrackId};

/// Which signal was measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SignalPoint {
    /// Every track, bus and return as it reaches its destination in the
    /// full mix — after its devices, automation, fader and pan, before it
    /// is summed there — and the master, all from one render of the whole
    /// mix (so buses and the master chain react to everything at once).
    Processed,
    /// Each track's clips as they play (clip gain, fades, warp, an
    /// instrument's notes) before any audio device, the fader or pan.
    Source,
}

impl SignalPoint {
    pub fn label(self) -> &'static str {
        match self {
            SignalPoint::Processed => "Processed (as mixed)",
            SignalPoint::Source => "Source (before processing)",
        }
    }

    /// What the measurements stand for, for the evidence.
    pub fn describe(self) -> &'static str {
        match self {
            SignalPoint::Processed => {
                "Measured in one offline render of the whole mix: each track, bus and return \
                 where it reaches its destination (after its devices, automation, fader and \
                 pan, before it is summed there), aligned with the master by the engine's own \
                 delay compensation; and the master."
            }
            SignalPoint::Source => {
                "Measured on each track's clips before any processing (no devices, fader or \
                 pan): this describes the material, not the mix."
            }
        }
    }
}

/// A stretch of time, seconds from the start of the analysed range.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimeSpan {
    pub start: f64,
    pub end: f64,
}

impl TimeSpan {
    pub fn new(start: f64, end: f64) -> Self {
        Self {
            start: start.min(end),
            end: start.max(end),
        }
    }

    pub fn seconds(&self) -> f64 {
        self.end - self.start
    }

    /// The overlap with another span (seconds).
    pub fn overlap(&self, other: &TimeSpan) -> f64 {
        (self.end.min(other.end) - self.start.max(other.start)).max(0.0)
    }
}

/// A frequency range (Hz).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrequencyRange {
    pub lo: f64,
    pub hi: f64,
}

impl FrequencyRange {
    pub fn centre(&self) -> f64 {
        (self.lo * self.hi).sqrt()
    }

    pub fn octaves(&self) -> f64 {
        (self.hi / self.lo.max(1e-9)).log2()
    }

    pub fn label(&self) -> String {
        format!("{}–{}", format_hz(self.lo), format_hz(self.hi))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category {
    Masking,
    Levels,
    Stereo,
    /// Technical faults and waste: DC offsets, overs and clipping inside
    /// device chains, devices that change nothing.
    Health,
    /// Differences to a reference track (a comparison, not a standard).
    Reference,
    /// Phase, polarity and timing between tracks.
    Phase,
    Dynamics,
}

impl Category {
    pub const ALL: [Category; 7] = [
        Category::Masking,
        Category::Levels,
        Category::Stereo,
        Category::Health,
        Category::Reference,
        Category::Phase,
        Category::Dynamics,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Category::Masking => "Masking",
            Category::Levels => "Levels",
            Category::Stereo => "Stereo and Mono",
            Category::Health => "Health Check",
            Category::Reference => "Reference",
            Category::Phase => "Phase and Timing",
            Category::Dynamics => "Dynamics",
        }
    }
}

/// The rule that made a finding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RuleId {
    SpectralMasking,
    MasterTruePeak,
    ClippedSamples,
    OverFullScale,
    NonFiniteSamples,
    SectionLoudnessStep,
    MonoFoldDownLoss,
    InvertedChannel,
    DcOffset,
    DeviceOverFullScale,
    DeviceClipping,
    DeviceInvalidSamples,
    DeviceSilences,
    DeviceNoChange,
    DevicesBypassed,
    ReferenceLoudness,
    ReferenceSpectrum,
    ReferenceSectionBalance,
    ReferenceStereo,
    /// Two tracks sharing a signal the other way up.
    PolarityInverted,
    /// Two tracks sharing a signal a little apart in time: comb filtering.
    TimeOffset,
    /// Two tracks cancelling in some bands, not by one delay or polarity.
    FrequencyDependentPhase,
    /// One track's onsets consistently before or after another's.
    OnsetOffset,
    /// Doubled parts whose onsets scatter against each other.
    LooseTiming,
    /// A wide (or off-centre) low end.
    WideLowEnd,
    /// The master's balance changing with frequency in mono.
    MonoBalanceShift,
    /// Attacks standing less far over their sustain after processing.
    TransientFlattening,
    /// A track's loudness range much wider than usual and than the mix's.
    WideLoudnessRange,
    /// A track's level stepping up or down while the mix does not.
    LevelJump,
    /// Two tracks carrying the same audio (not masking each other there).
    SameAudio,
}

impl RuleId {
    /// Found by the second stage ([`crate::diagnose_extra`]: pairs over
    /// excerpts, sources beside the processed signals), not from the
    /// profiles alone.
    pub fn second_stage(self) -> bool {
        matches!(
            self,
            RuleId::PolarityInverted
                | RuleId::TimeOffset
                | RuleId::FrequencyDependentPhase
                | RuleId::OnsetOffset
                | RuleId::LooseTiming
                | RuleId::TransientFlattening
        )
    }

    pub fn category(self) -> Category {
        match self {
            RuleId::SpectralMasking => Category::Masking,
            RuleId::MasterTruePeak
            | RuleId::ClippedSamples
            | RuleId::OverFullScale
            | RuleId::NonFiniteSamples
            | RuleId::SectionLoudnessStep => Category::Levels,
            RuleId::MonoFoldDownLoss | RuleId::InvertedChannel => Category::Stereo,
            RuleId::DcOffset
            | RuleId::DeviceOverFullScale
            | RuleId::DeviceClipping
            | RuleId::DeviceInvalidSamples
            | RuleId::DeviceSilences
            | RuleId::DeviceNoChange
            | RuleId::DevicesBypassed => Category::Health,
            RuleId::ReferenceLoudness
            | RuleId::ReferenceSpectrum
            | RuleId::ReferenceSectionBalance
            | RuleId::ReferenceStereo => Category::Reference,
            RuleId::PolarityInverted
            | RuleId::TimeOffset
            | RuleId::FrequencyDependentPhase
            | RuleId::OnsetOffset
            | RuleId::LooseTiming => Category::Phase,
            RuleId::WideLowEnd | RuleId::MonoBalanceShift => Category::Stereo,
            RuleId::TransientFlattening | RuleId::WideLoudnessRange | RuleId::LevelJump => {
                Category::Dynamics
            }
            RuleId::SameAudio => Category::Masking,
        }
    }

    /// The rule's version (with [`crate::ALGORITHM_VERSION`]: which
    /// calculation a finding came from).
    pub fn version(self) -> u32 {
        1
    }

    pub fn name(self) -> &'static str {
        match self {
            RuleId::SpectralMasking => "Spectral masking",
            RuleId::MasterTruePeak => "Master true peak",
            RuleId::ClippedSamples => "Clipped samples",
            RuleId::OverFullScale => "Over full scale",
            RuleId::NonFiniteSamples => "Invalid samples",
            RuleId::SectionLoudnessStep => "Section loudness step",
            RuleId::MonoFoldDownLoss => "Mono fold-down loss",
            RuleId::InvertedChannel => "Inverted channel",
            RuleId::DcOffset => "DC offset",
            RuleId::DeviceOverFullScale => "Over full scale between devices",
            RuleId::DeviceClipping => "Device flattens peaks",
            RuleId::DeviceInvalidSamples => "Device produces invalid samples",
            RuleId::DeviceSilences => "Device silences its input",
            RuleId::DeviceNoChange => "Device changes nothing",
            RuleId::DevicesBypassed => "Bypassed devices",
            RuleId::ReferenceLoudness => "Loudness against the reference",
            RuleId::ReferenceSpectrum => "Spectral balance against the reference",
            RuleId::ReferenceSectionBalance => "Section balance against the reference",
            RuleId::ReferenceStereo => "Stereo and mono against the reference",
            RuleId::PolarityInverted => "Polarity between tracks",
            RuleId::TimeOffset => "Time offset between tracks",
            RuleId::FrequencyDependentPhase => "Frequency-dependent phase",
            RuleId::OnsetOffset => "Onset offset",
            RuleId::LooseTiming => "Loose timing",
            RuleId::WideLowEnd => "Wide low end",
            RuleId::MonoBalanceShift => "Mono balance shift",
            RuleId::TransientFlattening => "Possible transient flattening",
            RuleId::WideLoudnessRange => "Wide loudness range",
            RuleId::LevelJump => "Level jump",
            RuleId::SameAudio => "Same audio",
        }
    }

    /// How the rule decides (its calculation, in words).
    pub fn method(self) -> &'static str {
        match self {
            RuleId::SpectralMasking => {
                "Per frame (Hann, half overlap) and third-octave band: the target's band counts \
                 when it is above the floor and within the prominence depth of its loudest band; \
                 the other track covers it by how close its band level comes (none 12 dB or more \
                 under, fully at the target's level or above), weighted by how alike their \
                 stereo placements are. Bands covered for a large enough share of the time are \
                 grouped into ranges; covered frames into events. Severity = role weight × \
                 √persistence × depth × importance; confidence from the signal measured, the \
                 roles known, how much evidence there is and how far above the floor it lies."
            }
            RuleId::MasterTruePeak => {
                "The master's true peak (4× oversampled, BS.1770 Annex 2) against the delivery \
                 ceiling (−1 dBTP: EBU R128, most streaming services)."
            }
            RuleId::ClippedSamples => {
                "Runs of three or more samples at full scale (overs) in the master's output."
            }
            RuleId::OverFullScale => {
                "Samples above 0 dBFS in a track's contribution at the master output (floating \
                 point keeps them; they clip where the signal is converted or limited)."
            }
            RuleId::NonFiniteSamples => {
                "Samples that are not numbers (NaN or infinity) in a rendered signal."
            }
            RuleId::SectionLoudnessStep => {
                "Integrated loudness (BS.1770) of each section of the master; adjacent sections \
                 differing by the step threshold or more."
            }
            RuleId::MonoFoldDownLoss => {
                "Per band, the mono fold-down ((L+R)/2) against what it would hold if left and \
                 right added without cancelling ((√L + √R)²/4): a panned source loses nothing, \
                 uncorrelated channels 3 dB, inverted ones everything. Bands within the \
                 relevance depth of the loudest band losing more than the threshold (half of it \
                 below the low-end frequency) for long enough are reported."
            }
            RuleId::InvertedChannel => {
                "Correlation of left and right over the frames where both carry the signal at \
                 similar levels, at or under the threshold."
            }
            RuleId::DcOffset => {
                "Each channel's mean over the analysed range (its DC offset), at or above the \
                 threshold (dBFS of its magnitude), on a track, bus, return or the master as it \
                 reaches its destination."
            }
            RuleId::DeviceOverFullScale => {
                "A probe after every device measures its output against its input (aligned by \
                 the engine's delay compensation). Reported where a device's output goes above \
                 0 dBFS — while its input did not, or more than 1 dB higher — and another \
                 device follows it on the track."
            }
            RuleId::DeviceClipping => {
                "Runs of three or more samples of exactly the same magnitude, at −12 dBFS or \
                 more, at a device's output: at least the threshold's number of runs, and the \
                 ratio's times as many as at its input."
            }
            RuleId::DeviceInvalidSamples => {
                "Samples that are not numbers (NaN or infinity) at a device's output while none \
                 reached its input: the first device on the way that produced them."
            }
            RuleId::DeviceSilences => {
                "A device whose input carries signal (RMS above the signal threshold) and whose \
                 output is silent (under the silence threshold) over the whole range."
            }
            RuleId::DeviceNoChange => {
                "A device that is not bypassed, whose output differs from its (latency-aligned) \
                 input by less than the no-change threshold relative to the input, over the \
                 whole range; its processing time is measured during the analysis render \
                 (the engine's per-node timing). Severity rises with that cost."
            }
            RuleId::DevicesBypassed => {
                "Devices bypassed throughout (hard bypass: FaderFrame passes the audio by them)."
            }
            RuleId::ReferenceLoudness => {
                "Integrated loudness, loudness range (EBU R128 / BS.1770, both channels weighted \
                 1) and peak-to-loudness ratio (true peak − integrated) of the master and of the \
                 reference as heard; reported when one differs by the threshold or more."
            }
            RuleId::ReferenceSpectrum => {
                "The long-term band levels (third octaves, mean power over the frames with \
                 signal) of the master and of the reference brought to the master's integrated \
                 loudness; their difference smoothed over neighbouring bands. Runs of adjacent \
                 bands differing by the threshold or more, in the same direction, among bands \
                 that carry energy in either."
            }
            RuleId::ReferenceSectionBalance => {
                "Per section of the mix: each broad region's share of the section's energy, \
                 against the reference's (the same stretch when the reference follows the \
                 timeline, else all of it). Reported where a region differs by the threshold or \
                 more and stands out by half that from the sections' typical difference (their \
                 median) — a difference the whole song has is the spectral finding's."
            }
            RuleId::ReferenceStereo => {
                "Per broad region, the mono fold-down change and the width (side against mid \
                 power) of the master and of the reference; reported where the mix loses the \
                 threshold more in mono, or is the width threshold wider or narrower."
            }
            RuleId::PolarityInverted => {
                "On an excerpt where both play: Auto-Align's estimator (maximum-likelihood \
                 weighted cross-correlation of the mono fold-downs, its peak's sign, a robust \
                 z-score's confidence) finds the second track the other way up; per band the \
                 two tracks' sum is set against the sum of their energies, now and with the \
                 polarity (and any delay found) corrected. Reported when the correction would \
                 bring back at least the threshold over the bands both play in."
            }
            RuleId::TimeOffset => {
                "Auto-Align's estimator finds a clear correlation peak (confidence and clarity \
                 over their thresholds) at a delay between the minimum and the comb-filter \
                 limit; the sum of the two cancels where the delay makes them half a period \
                 apart (the first notch at 1/(2·delay), then every 1/delay). Reported when \
                 moving one track by the delay (to a fraction of a sample) would bring back at \
                 least the threshold, or lift a cancelling band."
            }
            RuleId::FrequencyDependentPhase => {
                "Per band: the debiased coherence of the two mono fold-downs (they share a \
                 signal there), and their sum at least the threshold under the sum of their \
                 energies after the broadband correction (if one is suggested) — a phase \
                 relationship that changes with frequency (filters, different microphones or \
                 cabinets, all-pass shifts) or a steady tonal one. Contiguous bands make a \
                 range."
            }
            RuleId::OnsetOffset => {
                "FaderFrame's onset detector (spectral flux, attack starts) on both tracks' \
                 mono fold-downs over the excerpt; onsets paired one to one, nearest first, \
                 within the matching window. Reported when enough pair up and their median \
                 offset is over the flam threshold with a small spread (median absolute \
                 deviation): one plays consistently later."
            }
            RuleId::LooseTiming => {
                "The same onset pairing between parts said to be doubles (the same role, or \
                 selected together): reported when the offsets' spread (median absolute \
                 deviation) is at least the threshold."
            }
            RuleId::WideLowEnd => {
                "Per band below the low-end frequency: the side signal's power against the \
                 mid's ((L−R)/2 against (L+R)/2), long-term and per frame; bands within the \
                 relevance depth whose side comes within the threshold of their mid for long \
                 enough. Ranges already reported as a mono fold-down loss are left to that \
                 rule."
            }
            RuleId::MonoBalanceShift => {
                "On the master, per band: the mono fold-down's power against the channels' \
                 mean power, relative to the same for all relevant bands together — how the \
                 mix's tonal balance moves in mono. Contiguous bands moving down by the \
                 threshold or more make a range; the tracks bringing the most side signal \
                 there (each measured alone) are named."
            }
            RuleId::TransientFlattening => {
                "The track's envelope events (5 ms RMS rising over its 50 ms minimum) are found \
                 on its source signal; at each, the transient-to-sustain ratio (highest peak in \
                 30 ms over the RMS 60–200 ms later) is measured on the source and on the \
                 processed signal at the same moment. Reported as possible when the source had \
                 transients and the median change is a drop of the threshold or more; the \
                 crest factor and the attack rise are shown beside it."
            }
            RuleId::WideLoudnessRange => {
                "The track's loudness range (EBU Tech 3342: the 10th to 95th percentile of its \
                 gated short-term loudness) over the threshold and wider than the mix's by the \
                 margin, over enough active time."
            }
            RuleId::LevelJump => {
                "The median momentary loudness (BS.1770, 400 ms) over the 2 s after a moment \
                 against the 2 s before, both above the track's own gate (−70 LUFS, 20 LU \
                 under its integrated loudness): changes of the threshold or more that the \
                 mix does not make by at least the mirrored share."
            }
            RuleId::SameAudio => {
                "From each track's per-frame band sums of its mono fold-down, window by window \
                 (4 s, sliding by half) over the frames both play in: per third-octave band the \
                 magnitude-squared coherence of the two (debiased), a small delay between them \
                 found and allowed for; where the bands that matter are coherent (weighted and \
                 plain mean over the threshold, two bands at least) the two carry the same \
                 audio, linearly related (gain, pan, EQ, a small delay). The masking rule leaves \
                 those stretches out; this reports them, with the level between the copies and \
                 their mono sum."
            }
        }
    }

    /// What the rule cannot tell, and when it is known to be wrong.
    pub fn limitations(self) -> &'static [&'static str] {
        match self {
            RuleId::SpectralMasking => &[
                "Overlap of level and time in a band is evidence of a possible conflict, not a \
                 psychoacoustic masking model: whether it matters is a musical judgement.",
                "Two microphones on one source, doubled parts and layered sounds overlap by \
                 design; give the tracks roles to tell the analysis what they are. Stretches \
                 where the two carry the same audio (coherent bands: a copy, one source \
                 processed two ways) are left out and reported as such.",
                "Below about 40 Hz third-octave bands blend into their neighbours at this \
                 resolution (the analysis window's main lobe is about 23 Hz wide).",
                "Processed signals are measured where each track reaches its bus or the \
                 master, before the processing there: what a bus compressor or the master \
                 chain does to the sum is in the bus's and the master's measurements, not the \
                 track's.",
                "A track is not compared with a bus or return it reaches (that carries the \
                 track itself, or its effect); a bus summing other tracks is not compared \
                 either — its tracks are.",
                "Phase interaction between the tracks is not measured.",
            ],
            RuleId::MasterTruePeak => &[
                "A ceiling of −1 dBTP is a delivery convention; a premaster for further \
                 mastering may sit anywhere.",
            ],
            RuleId::ClippedSamples => {
                &["A deliberately clipped sound (or a square wave) reads as overs too."]
            }
            RuleId::OverFullScale => &[
                "FaderFrame mixes in floating point: nothing clips inside the mix itself; the \
                 master chain may bring it back under full scale.",
            ],
            RuleId::NonFiniteSamples => &["The device that produced them is not identified."],
            RuleId::SectionLoudnessStep => &[
                "Large steps between sections are often intended (a quiet bridge, a breakdown).",
                "Sections shorter than four seconds are left out.",
            ],
            RuleId::MonoFoldDownLoss => &[
                "Wide stereo (reverbs, wide synths) loses some level in mono by nature; a loss \
                 says how mono playback changes the band, not that the stereo image is wrong.",
                "Low correlation alone is not a fault.",
            ],
            RuleId::InvertedChannel => &[
                "A wide stereo effect can correlate negatively without either channel being \
                 inverted; check by flipping one channel's polarity and listening.",
            ],
            RuleId::DcOffset => &[
                "Very low notes (under 20 Hz) over a short range move the mean too.",
                "A DC offset on a bus or the master comes from something reaching it: the \
                 tracks' own findings say which.",
            ],
            RuleId::DeviceOverFullScale => &[
                "FaderFrame's own devices work in floating point and do not clip at 0 dBFS; \
                 devices modelled on hardware, or working in fixed point, may. Whether the \
                 next device clips is not measured here.",
                "Probes measure the range and up to the graph's latency after it.",
            ],
            RuleId::DeviceClipping => &[
                "Clippers, limiters and saturators flatten peaks by design: then this is their \
                 sound, not a fault.",
            ],
            RuleId::DeviceInvalidSamples => &[
                "A device fed invalid samples by an earlier one passes them on; only the first \
                 is reported.",
            ],
            RuleId::DeviceSilences => &[
                "A gate, a mute or a device waiting for its sidechain can be silent on \
                 purpose over the range analysed.",
            ],
            RuleId::DeviceNoChange => &[
                "Only the analysed range is measured: a de-esser, limiter or gate may act on \
                 material elsewhere, or on louder input.",
                "Processing time is measured offline, faster than real time and on several \
                 threads: a guide to the cost, not a live reading.",
            ],
            RuleId::DevicesBypassed => &[
                "A hard bypass costs (almost) no processing in FaderFrame; a hosted plugin's \
                 instance stays loaded (memory, its own threads).",
            ],
            RuleId::ReferenceLoudness => &[
                "A mix before mastering is expected to be quieter than a mastered reference; \
                 every other comparison is loudness-matched for that reason.",
            ],
            RuleId::ReferenceSpectrum => &[
                "A reference is a comparison target, not a standard: a different arrangement, \
                 key or genre has a different spectrum for good reasons.",
                "Loudness matching uses BS.1770's weighting; the band levels compared are not \
                 weighted.",
            ],
            RuleId::ReferenceSectionBalance => &[
                "A reference that does not follow the timeline is compared as a whole with \
                 each section: its own sections differ among themselves too.",
            ],
            RuleId::ReferenceStereo => &[
                "Width is a choice: a narrower or wider image than the reference's is a \
                 difference, not a fault.",
            ],
            RuleId::PolarityInverted => &[
                "Measured on one excerpt (the busiest where both play); the relationship can \
                 differ elsewhere.",
                "A deliberately inverted track (a snare bottom microphone already flipped the \
                 other way, a creative cancellation) reads the same.",
                "Processed signals are each rendered alone: shared buses and the master chain \
                 react to one track differently than to the mix.",
            ],
            RuleId::TimeOffset => &[
                "Some offsets are musical or part of the sound (a room microphone's distance, \
                 a doubling, a slapback): aligning everything to the highest correlation is \
                 not a goal. Listen before applying.",
                "One delay serves the whole excerpt; microphones that moved, or several \
                 sources reaching two microphones (bleed) at different delays, cannot all be \
                 aligned at once (the clarity says how alone the peak stands).",
                "Measured on one excerpt (the busiest where both play).",
            ],
            RuleId::FrequencyDependentPhase => &[
                "No single delay or polarity fixes a phase difference that changes with \
                 frequency; an all-pass filter can rotate one track's phase in a region, but \
                 the analysis does not design it: it is a suggestion to try by ear.",
                "Steady tones (a bass note under a kick's pitch) can cancel on one note and add \
                 on the next: this is the average over the excerpt.",
                "Third-octave bands: a comb's teeth narrower than a band average out.",
            ],
            RuleId::OnsetOffset => &[
                "Timing offsets are often the feel (a laid-back bass, a dragging double); the \
                 rule measures, it does not judge the groove.",
                "Onsets are found to about a millisecond; their pairing assumes the two play \
                 the same rhythm in the excerpt.",
                "A track delay moves every note by the same amount.",
            ],
            RuleId::LooseTiming => &[
                "Doubles are often loose on purpose (width, a human feel).",
                "Onsets are found to about a millisecond; only clear ones are paired.",
            ],
            RuleId::WideLowEnd => &[
                "A wide low end is not wrong by itself (ambient music, stereo synth basses); \
                 it matters for mono playback, subwoofers summed to mono, vinyl cutting and \
                 club systems.",
                "Low correlation alone is not a fault.",
            ],
            RuleId::MonoBalanceShift => &[
                "Wide parts are quieter in mono by nature; a shift says how a mono speaker \
                 changes the balance, not that the stereo mix is wrong.",
                "The tracks named are measured alone: their side signals can add or cancel in \
                 the mix.",
            ],
            RuleId::TransientFlattening => &[
                "Possible, not certain: compression, limiting, saturation, clipping, transient \
                 shaping, EQ and added sustain (reverb, delay) all lower the ratio; the rule \
                 does not identify the device or its settings.",
                "The processed signal is the track alone through its buses and the master \
                 chain: a master limiter reacts to one track differently than to the mix.",
                "Flattened attacks are often intended (a denser sound, control).",
            ],
            RuleId::WideLoudnessRange => &[
                "A wide range is often the arrangement (a quiet verse, a loud chorus) or the \
                 performance; the rule says it is unusual, not that it is wrong.",
                "The loudness range is a programme measure: sparse material inflates it, so \
                 short active times are left out.",
            ],
            RuleId::LevelJump => &[
                "Steps are often intended (an entry, a drop, automation).",
                "Momentary loudness is K-weighted: a change of tone (a filter opening) can \
                 read as a change of level.",
            ],
            RuleId::SameAudio => &[
                "Coherence says the two are linearly related, not why: a copy, one source \
                 processed two ways, or one source picked up by two microphones (where it \
                 dominates both).",
                "Copies delayed by more than about a millisecond, or changed by a time-varying \
                 or nonlinear effect (chorus, pitch shift, heavy distortion, a re-amp), read as \
                 different audio and are judged for masking as before.",
                "A stereo recording whose channels are largely uncorrelated (spaced \
                 microphones, stereo effects) is not recognised as one source this way.",
                "Judged in 4 s windows: the edges of a stretch are known to about 2 s.",
            ],
        }
    }
}

/// Units of measured values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    /// A level difference.
    Db,
    /// A band level: mean-square power in dB re full scale.
    DbBand,
    Dbfs,
    Dbtp,
    Lufs,
    Lu,
    Hz,
    Seconds,
    /// 0 to 1, shown as a percentage.
    Share,
    Count,
    /// −1 to 1.
    Correlation,
    Octaves,
    /// A device on the track (the value is its instance id; shown by name:
    /// [`Evidence::value_named`]).
    Device,
    /// Milliseconds (signed: later is positive).
    Ms,
    /// Samples at the project rate (fractional).
    Samples,
    Degrees,
    /// 0 to 1, shown as a number (coherence, confidence).
    Ratio,
}

/// One measured value behind a finding.
#[derive(Clone, Debug, PartialEq)]
pub struct Evidence {
    pub label: String,
    pub value: f64,
    pub unit: Unit,
    /// The track it was measured on (`None`: the pair, the mix).
    pub track: Option<TrackId>,
}

impl Evidence {
    pub fn new(label: impl Into<String>, value: f64, unit: Unit, track: Option<TrackId>) -> Self {
        Self {
            label: label.into(),
            value: if value.is_finite() { value } else { 0.0 },
            unit,
            track,
        }
    }

    /// The value with its unit, rounded to what the measurement can tell.
    pub fn value_text(&self) -> String {
        let v = self.value;
        match self.unit {
            Unit::Db => format!("{v:+.1} dB"),
            Unit::DbBand => format!("{v:.1} dB"),
            Unit::Dbfs => format!("{v:.1} dBFS"),
            Unit::Dbtp => format!("{v:+.1} dBTP"),
            Unit::Lufs => format!("{v:.1} LUFS"),
            Unit::Lu => format!("{v:.1} LU"),
            Unit::Hz => format_hz(v),
            Unit::Seconds => format!("{v:.1} s"),
            Unit::Share => format!("{:.0} %", v * 100.0),
            Unit::Count => format!("{v:.0}"),
            Unit::Correlation => format!("{v:+.2}"),
            Unit::Octaves => format!("{v:.2} oct"),
            Unit::Ms => format!("{v:+.2} ms"),
            Unit::Samples => format!("{v:+.2} samples"),
            Unit::Degrees => format!("{v:+.0}°"),
            Unit::Ratio => format!("{v:.2}"),
            Unit::Device => "a device".into(),
        }
    }

    /// The device it names ([`Unit::Device`]).
    pub fn device(&self) -> Option<PluginInstanceId> {
        (self.unit == Unit::Device && self.value >= 0.0)
            .then_some(PluginInstanceId(self.value as u64))
    }

    /// The value with its unit, devices by name.
    pub fn value_named(&self, device: &dyn Fn(PluginInstanceId) -> String) -> String {
        match self.device() {
            Some(d) => device(d),
            None => self.value_text(),
        }
    }
}

/// A term of a severity or confidence calculation (shown with it).
#[derive(Clone, Debug, PartialEq)]
pub struct Term {
    pub name: &'static str,
    pub value: f64,
}

/// Three steps a score is shown in (not a measurement of quality).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Grade {
    Low,
    Moderate,
    High,
}

impl Grade {
    /// Severity: high from 0.5, moderate from 0.25.
    pub fn of_severity(v: f64) -> Self {
        if v >= 0.5 {
            Grade::High
        } else if v >= 0.25 {
            Grade::Moderate
        } else {
            Grade::Low
        }
    }

    /// Confidence: high from 0.75, moderate from 0.5.
    pub fn of_confidence(v: f64) -> Self {
        if v >= 0.75 {
            Grade::High
        } else if v >= 0.5 {
            Grade::Moderate
        } else {
            Grade::Low
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Grade::Low => "Low",
            Grade::Moderate => "Moderate",
            Grade::High => "High",
        }
    }
}

/// How an interaction is spread over time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pattern {
    /// Most of the time the source plays there.
    Persistent,
    /// Three or more separate events.
    Recurring,
    /// One or two events.
    Isolated,
}

impl Pattern {
    pub fn label(self) -> &'static str {
        match self {
            Pattern::Persistent => "persistent",
            Pattern::Recurring => "recurring",
            Pattern::Isolated => "isolated",
        }
    }
}

/// Band levels behind a finding (dB per band, per track), the range it
/// is about highlighted.
#[derive(Clone, Debug, PartialEq)]
pub struct SpectrumEvidence {
    pub centres: Vec<f64>,
    pub series: Vec<(TrackId, Vec<f64>)>,
    /// Bands `a..=b`.
    pub highlight: (usize, usize),
}

/// A processing experiment a finding suggests (see [`Recommendation`]).
#[derive(Clone, Debug, PartialEq)]
pub enum Experiment {
    /// A bell on the built-in EQ, inserted at the end of the track's
    /// devices.
    EqCut {
        track: TrackId,
        freq_hz: f64,
        gain_db: f64,
        q: f64,
    },
    /// A dynamic bell on the built-in EQ that cuts up to `range_db` only
    /// while `key` plays in its region (the EQ's sidechain from `key`).
    DynamicEqCut {
        track: TrackId,
        key: TrackId,
        freq_hz: f64,
        range_db: f64,
        q: f64,
    },
    /// The track's fader moved by `change_db`.
    Level { track: TrackId, change_db: f64 },
    /// A low cut on the side signal (the built-in EQ's band on Side, 24
    /// dB/oct): the low end below `freq_hz` made mono.
    SideLowCut { track: TrackId, freq_hz: f64 },
    /// Something to check or try by hand (nothing is changed).
    Manual { text: String },
    /// Gain staging around a device: the built-in Utility before it at
    /// `trim_db` (less level into it) and one after it at −`trim_db` (the
    /// level given back), so the device is driven less while the track's
    /// level stays where it was (exactly, for a device that is linear).
    GainStage {
        track: TrackId,
        device: PluginInstanceId,
        trim_db: f64,
    },
    /// The built-in Utility's DC filter at the end of the track's devices.
    DcFilter { track: TrackId },
    /// The device bypassed.
    Bypass {
        track: TrackId,
        device: PluginInstanceId,
    },
    /// Hear the reference instead of the mix, loudness-matched (A/B in the
    /// monitor path; nothing in the project changes).
    CompareReference { reference: ReferenceId },
    /// The reference's spectrum as EQ Match's reference in the first
    /// built-in EQ of `track` (one is added when there is none), its editor
    /// opened on EQ Match: matching is then done (or not) by hand there.
    ReferenceEqMatch {
        reference: ReferenceId,
        track: TrackId,
    },
    /// The track's own delay moved by `change_ms` (to a fraction of a
    /// sample; negative: earlier) and, with `invert`, its polarity turned
    /// over (`Track::delay_ms`, `Track::phase_invert`).
    Alignment {
        track: TrackId,
        change_ms: f64,
        invert: bool,
    },
}

/// Limits experiments are kept within (conservative by design).
pub const MAX_CUT_DB: f64 = 6.0;
pub const MAX_LEVEL_DB: f64 = 6.0;
/// The deepest trim gain staging puts before a device.
pub const MAX_TRIM_DB: f64 = 12.0;
/// The largest track-delay move an experiment makes (ms).
pub const MAX_ALIGN_MS: f64 = 30.0;

impl Experiment {
    /// The track it changes.
    pub fn track(&self) -> Option<TrackId> {
        match self {
            Experiment::EqCut { track, .. }
            | Experiment::DynamicEqCut { track, .. }
            | Experiment::Level { track, .. }
            | Experiment::SideLowCut { track, .. }
            | Experiment::GainStage { track, .. }
            | Experiment::DcFilter { track }
            | Experiment::Bypass { track, .. }
            | Experiment::Alignment { track, .. }
            | Experiment::ReferenceEqMatch { track, .. } => Some(*track),
            Experiment::Manual { .. } | Experiment::CompareReference { .. } => None,
        }
    }

    /// The device it changes or works around.
    pub fn device(&self) -> Option<PluginInstanceId> {
        match self {
            Experiment::GainStage { device, .. } | Experiment::Bypass { device, .. } => {
                Some(*device)
            }
            _ => None,
        }
    }

    /// An experiment that hands over to another tool instead of making a
    /// candidate (what its button says): the A/B comparison with a
    /// reference, EQ Match.
    pub fn hand_off(&self) -> Option<&'static str> {
        match self {
            Experiment::CompareReference { .. } => Some("Listen A/B"),
            Experiment::ReferenceEqMatch { .. } => Some("Send to EQ Match"),
            _ => None,
        }
    }

    /// Every track it involves (a dynamic cut's key too).
    pub fn tracks(&self) -> Vec<TrackId> {
        match self {
            Experiment::DynamicEqCut { track, key, .. } => vec![*track, *key],
            _ => self.track().into_iter().collect(),
        }
    }

    /// It can be auditioned and applied (not a manual suggestion, not a
    /// hand-off to another tool: [`Self::hand_off`]).
    pub fn previewable(&self) -> bool {
        !matches!(
            self,
            Experiment::Manual { .. }
                | Experiment::CompareReference { .. }
                | Experiment::ReferenceEqMatch { .. }
        )
    }

    /// Check the values are within the conservative limits and the
    /// frequency below `nyquist`.
    pub fn validate(&self, nyquist: f64) -> Result<(), String> {
        let freq = |f: f64| {
            if f.is_finite() && (20.0..nyquist.min(20_000.0)).contains(&f) {
                Ok(())
            } else {
                Err(format!("{f:.0} Hz is outside the audible range"))
            }
        };
        let q = |q: f64| {
            if q.is_finite() && (0.3..=10.0).contains(&q) {
                Ok(())
            } else {
                Err(format!("a Q of {q:.2} is outside 0.3 to 10"))
            }
        };
        let cut = |g: f64| {
            if g.is_finite() && (-MAX_CUT_DB..0.0).contains(&g) {
                Ok(())
            } else {
                Err(format!(
                    "a cut of {g:.1} dB is outside 0 to −{MAX_CUT_DB} dB"
                ))
            }
        };
        match self {
            Experiment::EqCut {
                freq_hz,
                gain_db,
                q: qv,
                ..
            } => freq(*freq_hz).and(q(*qv)).and(cut(*gain_db)),
            Experiment::DynamicEqCut {
                track,
                key,
                freq_hz,
                range_db,
                q: qv,
            } => {
                if track == key {
                    return Err("a track cannot key its own cut".into());
                }
                freq(*freq_hz).and(q(*qv)).and(cut(*range_db))
            }
            Experiment::Level { change_db, .. } => {
                if change_db.is_finite() && change_db.abs() <= MAX_LEVEL_DB && *change_db != 0.0 {
                    Ok(())
                } else {
                    Err(format!(
                        "a level change of {change_db:.1} dB is not allowed"
                    ))
                }
            }
            Experiment::SideLowCut { freq_hz, .. } => {
                if freq_hz.is_finite() && (30.0..=300.0).contains(freq_hz) {
                    Ok(())
                } else {
                    Err(format!(
                        "a side low cut at {freq_hz:.0} Hz is outside 30 to 300 Hz"
                    ))
                }
            }
            Experiment::GainStage { trim_db, .. } => {
                if trim_db.is_finite() && (-MAX_TRIM_DB..0.0).contains(trim_db) {
                    Ok(())
                } else {
                    Err(format!(
                        "a trim of {trim_db:.1} dB is outside 0 to −{MAX_TRIM_DB} dB"
                    ))
                }
            }
            Experiment::Manual { .. }
            | Experiment::DcFilter { .. }
            | Experiment::Bypass { .. }
            | Experiment::CompareReference { .. }
            | Experiment::ReferenceEqMatch { .. } => Ok(()),
            Experiment::Alignment {
                change_ms, invert, ..
            } => {
                if !change_ms.is_finite() || change_ms.abs() > MAX_ALIGN_MS {
                    Err(format!(
                        "a delay change of {change_ms:.2} ms is outside ±{MAX_ALIGN_MS} ms"
                    ))
                } else if *change_ms == 0.0 && !invert {
                    Err("the alignment changes nothing".into())
                } else {
                    Ok(())
                }
            }
        }
    }

    /// What it does, with the names tracks have now.
    pub fn title(&self, name: &dyn Fn(TrackId) -> String) -> String {
        self.title_named(name, &|_| "a device".into())
    }

    /// [`Self::title`] with devices named too.
    pub fn title_named(
        &self,
        name: &dyn Fn(TrackId) -> String,
        device: &dyn Fn(PluginInstanceId) -> String,
    ) -> String {
        match self {
            Experiment::GainStage {
                track,
                device: d,
                trim_db,
            } => format!(
                "Gain staging around {} on {}: {trim_db:+.1} dB into it, {:+.1} dB after it",
                device(*d),
                name(*track),
                -trim_db
            ),
            Experiment::DcFilter { track } => {
                format!("DC filter at the end of {}'s devices", name(*track))
            }
            Experiment::Bypass { track, device: d } => {
                format!("Bypass {} on {}", device(*d), name(*track))
            }
            Experiment::CompareReference { .. } => {
                "Listen: the mix against the reference, loudness-matched".into()
            }
            Experiment::ReferenceEqMatch { track, .. } => format!(
                "EQ Match on {}: the reference's spectrum as the target (set by hand there)",
                name(*track)
            ),
            Experiment::EqCut {
                track,
                freq_hz,
                gain_db,
                q,
            } => format!(
                "EQ cut on {}: {gain_db:.1} dB at {} (Q {q:.1})",
                name(*track),
                format_hz(*freq_hz)
            ),
            Experiment::DynamicEqCut {
                track,
                key,
                freq_hz,
                range_db,
                q,
            } => format!(
                "Dynamic EQ on {}: up to {range_db:.1} dB at {} (Q {q:.1}) while {} plays",
                name(*track),
                format_hz(*freq_hz),
                name(*key)
            ),
            Experiment::Level { track, change_db } => {
                format!("Level of {}: {change_db:+.1} dB", name(*track))
            }
            Experiment::SideLowCut { track, freq_hz } => format!(
                "Mono low end on {}: side signal cut below {}",
                name(*track),
                format_hz(*freq_hz)
            ),
            Experiment::Manual { text } => format!("Check by hand: {text}"),
            Experiment::Alignment {
                track,
                change_ms,
                invert,
            } => {
                let when = if *change_ms < 0.0 { "earlier" } else { "later" };
                match (*change_ms != 0.0, *invert) {
                    (true, true) => format!(
                        "{} {:.3} ms {when} and its polarity inverted",
                        name(*track),
                        change_ms.abs()
                    ),
                    (true, false) => {
                        format!("{} {:.3} ms {when}", name(*track), change_ms.abs())
                    }
                    _ => format!("{}'s polarity inverted", name(*track)),
                }
            }
        }
    }
}

/// A suggested experiment and why (see [`Experiment`]). Never a promise
/// that the mix gets better: something to listen to.
#[derive(Clone, Debug, PartialEq)]
pub struct Recommendation {
    pub experiment: Experiment,
    /// What it is meant to do.
    pub purpose: String,
    /// What else it does, and what is uncertain.
    pub side_effects: String,
}

/// What a finding is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Subject {
    /// `masker` may cover `target` (the one that suffers).
    Pair { target: TrackId, masker: TrackId },
    /// One track's signal.
    Track(TrackId),
    /// The master output (its track, when known).
    Master(Option<TrackId>),
    /// Two tracks heard together, neither suffering more (phase and
    /// timing between them; `a` before `b` in the analysis' order).
    Tracks { a: TrackId, b: TrackId },
}

/// A finding (see the module docs). Ids are stable: the same rule finding
/// the same thing on the same tracks in the same band gets the same id in
/// the next analysis (dismissals stay dismissed).
#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    pub id: u64,
    pub rule: RuleId,
    pub signal: SignalPoint,
    pub subject: Subject,
    /// Tracks it involves (the one that suffers first).
    pub tracks: Vec<TrackId>,
    /// From the first event's start to the last's end.
    pub time: Option<TimeSpan>,
    /// Where it happens (merged; at most [`MAX_EVENTS`]).
    pub events: Vec<TimeSpan>,
    pub pattern: Option<Pattern>,
    pub frequency: Option<FrequencyRange>,
    /// 0 to 1 (see [`Grade::of_severity`]); `severity_terms` say from what.
    pub severity: f64,
    pub severity_terms: Vec<Term>,
    /// 0 to 1 (see [`Grade::of_confidence`]).
    pub confidence: f64,
    pub confidence_terms: Vec<Term>,
    /// Measured values (facts, with units).
    pub evidence: Vec<Evidence>,
    pub spectrum: Option<SpectrumEvidence>,
    /// Sections (by name) most of the events fall in.
    pub sections: Vec<String>,
    pub recommendations: Vec<Recommendation>,
    /// Things to keep in mind about this one (beyond the rule's
    /// limitations).
    pub caveats: Vec<String>,
}

/// Events kept per finding (the rest merged into the last).
pub const MAX_EVENTS: usize = 64;

impl Finding {
    pub fn category(&self) -> Category {
        self.rule.category()
    }

    /// How findings are ranked: severity, weighed by confidence
    /// (`severity × (0.5 + 0.5 × confidence)`).
    pub fn relevance(&self) -> f64 {
        self.severity * (0.5 + 0.5 * self.confidence)
    }

    fn evidence_value(&self, label: &str) -> Option<f64> {
        self.evidence
            .iter()
            .find(|e| e.label == label)
            .map(|e| e.value)
    }

    /// The device it is about (health findings: [`Unit::Device`] evidence).
    pub fn device(&self) -> Option<PluginInstanceId> {
        self.evidence.iter().find_map(Evidence::device)
    }

    /// A short title from the evidence, with the names tracks have now.
    pub fn title(&self, name: &dyn Fn(TrackId) -> String) -> String {
        self.title_named(name, &|_| "A device".into())
    }

    /// [`Self::title`] with the device it is about named.
    pub fn title_named(
        &self,
        name: &dyn Fn(TrackId) -> String,
        device: &dyn Fn(PluginInstanceId) -> String,
    ) -> String {
        let dev = || self.device().map_or_else(|| "A device".into(), device);
        let range = self.frequency.map(|f| f.label()).unwrap_or_default();
        let on = |s: &Subject| match s {
            Subject::Track(t) => name(*t),
            Subject::Master(Some(t)) => name(*t),
            Subject::Master(None) => "the master".into(),
            Subject::Pair { target, .. } => name(*target),
            Subject::Tracks { a, .. } => name(*a),
        };
        let (a, b) = match self.subject {
            Subject::Tracks { a, b }
            | Subject::Pair {
                target: a,
                masker: b,
            } => (name(a), name(b)),
            _ => (String::new(), String::new()),
        };
        // The later of the two by a signed lag (positive: `b` later).
        let later = |lag: f64| {
            if lag >= 0.0 {
                (b.clone(), a.clone())
            } else {
                (a.clone(), b.clone())
            }
        };
        match self.rule {
            RuleId::SpectralMasking => match self.subject {
                Subject::Pair { target, masker } => {
                    format!("{} may mask {} at {range}", name(masker), name(target))
                }
                _ => format!("Possible masking at {range}"),
            },
            RuleId::MasterTruePeak => format!(
                "{}: true peak {:+.1} dBTP",
                on(&self.subject),
                self.evidence_value(labels::TRUE_PEAK).unwrap_or(0.0)
            ),
            RuleId::ClippedSamples => format!(
                "{}: {:.0} clipped runs",
                on(&self.subject),
                self.evidence_value(labels::OVERS).unwrap_or(0.0)
            ),
            RuleId::OverFullScale => format!("{} goes over 0 dBFS", on(&self.subject)),
            RuleId::NonFiniteSamples => format!("{} has invalid samples", on(&self.subject)),
            RuleId::SectionLoudnessStep => format!(
                "{}: {:.1} LU step between sections",
                on(&self.subject),
                self.evidence_value(labels::STEP).unwrap_or(0.0).abs()
            ),
            RuleId::MonoFoldDownLoss => format!(
                "{}: {:.1} dB lost in mono at {range}",
                on(&self.subject),
                -self.evidence_value(labels::FOLD_LOSS).unwrap_or(0.0)
            ),
            RuleId::InvertedChannel => {
                format!("{}: left and right look inverted", on(&self.subject))
            }
            RuleId::DcOffset => format!(
                "{}: DC offset at {:.0} dBFS",
                on(&self.subject),
                self.evidence_value(labels::DC).unwrap_or(0.0)
            ),
            RuleId::DeviceOverFullScale => format!(
                "{} on {}: {:+.1} dBFS into the next device",
                dev(),
                on(&self.subject),
                self.evidence_value(labels::OUT_PEAK).unwrap_or(0.0)
            ),
            RuleId::DeviceClipping => {
                format!("{} on {} flattens peaks", dev(), on(&self.subject))
            }
            RuleId::DeviceInvalidSamples => {
                format!(
                    "{} on {} produces invalid samples",
                    dev(),
                    on(&self.subject)
                )
            }
            RuleId::DeviceSilences => {
                format!("{} on {} silences its input", dev(), on(&self.subject))
            }
            RuleId::DeviceNoChange => {
                format!(
                    "{} on {} changes nothing measurable",
                    dev(),
                    on(&self.subject)
                )
            }
            RuleId::DevicesBypassed => {
                let n = self
                    .evidence
                    .iter()
                    .filter(|e| e.unit == Unit::Device)
                    .count();
                format!(
                    "{}: {n} bypassed device{}",
                    on(&self.subject),
                    if n == 1 { "" } else { "s" }
                )
            }
            RuleId::ReferenceLoudness => format!(
                "Loudness: the mix {:+.1} LU against the reference",
                self.evidence_value(labels::LOUDNESS_DIFFERENCE)
                    .unwrap_or(0.0)
            ),
            RuleId::ReferenceSpectrum => format!(
                "{:+.1} dB at {range} against the reference (loudness-matched)",
                self.evidence_value(labels::DIFFERENCE).unwrap_or(0.0)
            ),
            RuleId::ReferenceSectionBalance => format!(
                "{}: balance departs from the reference",
                self.sections.first().map_or("A section", String::as_str)
            ),
            RuleId::ReferenceStereo => "Stereo image differs from the reference".into(),
            RuleId::PolarityInverted => format!("{a} and {b} are of opposite polarity"),
            RuleId::TimeOffset => {
                let lag = self.evidence_value(labels::LAG).unwrap_or(0.0);
                let (l, e) = later(lag);
                format!("{l} is {:.2} ms behind {e} (comb filtering)", lag.abs())
            }
            RuleId::FrequencyDependentPhase => {
                format!("{a} and {b} partly cancel at {range}")
            }
            RuleId::OnsetOffset => {
                let lag = self.evidence_value(labels::ONSET_MEDIAN).unwrap_or(0.0);
                let (l, e) = later(lag);
                format!("{l} plays {:.0} ms after {e}", lag.abs())
            }
            RuleId::LooseTiming => format!("Loose timing between {a} and {b}"),
            RuleId::WideLowEnd => format!("{}: wide low end at {range}", on(&self.subject)),
            RuleId::MonoBalanceShift => format!(
                "{}: {range} {:.1} dB down against the rest in mono",
                on(&self.subject),
                -self.evidence_value(labels::MONO_SHIFT).unwrap_or(0.0)
            ),
            RuleId::TransientFlattening => format!(
                "{}: possible transient flattening ({:+.1} dB)",
                on(&self.subject),
                self.evidence_value(labels::TSR_CHANGE).unwrap_or(0.0)
            ),
            RuleId::WideLoudnessRange => format!(
                "{}: loudness range {:.1} LU",
                on(&self.subject),
                self.evidence_value(labels::LRA).unwrap_or(0.0)
            ),
            RuleId::LevelJump => format!(
                "{}: level jumps {:+.1} LU",
                on(&self.subject),
                self.evidence_value(labels::STEP).unwrap_or(0.0)
            ),
            RuleId::SameAudio => format!(
                "{a} and {b} carry the same audio for {:.0} s (coherence {:.2})",
                self.evidence_value(labels::SAME_SECONDS).unwrap_or(0.0),
                self.evidence_value(labels::COHERENCE).unwrap_or(0.0)
            ),
        }
    }

    /// The interpretation: what the evidence suggests (worded as a
    /// possibility, never as a verdict).
    pub fn explanation(&self, name: &dyn Fn(TrackId) -> String) -> String {
        self.explanation_named(name, &|_| "the device".into())
    }

    /// [`Self::explanation`] with the device it is about named.
    pub fn explanation_named(
        &self,
        name: &dyn Fn(TrackId) -> String,
        device: &dyn Fn(PluginInstanceId) -> String,
    ) -> String {
        let dev = || self.device().map_or_else(|| "the device".into(), device);
        let pct = |v: f64| format!("{:.0} %", v * 100.0);
        let range = self.frequency.map(|f| f.label()).unwrap_or_default();
        match (self.rule, self.subject) {
            (RuleId::SpectralMasking, Subject::Pair { target, masker }) => {
                let pattern = self.pattern.map_or("", |p| p.label());
                let secs = self.evidence_value(labels::CONFLICT_SECONDS).unwrap_or(0.0);
                let persistence = self.evidence_value(labels::PERSISTENCE).unwrap_or(0.0);
                let margin = self.evidence_value(labels::MARGIN).unwrap_or(0.0);
                let spatial = self.evidence_value(labels::SPATIAL).unwrap_or(1.0);
                let sections = if self.sections.is_empty() {
                    String::new()
                } else {
                    format!(" (mostly in {})", self.sections.join(", "))
                };
                let rel = if margin >= 0.0 {
                    format!("{margin:.1} dB above")
                } else {
                    format!("{:.1} dB under", -margin)
                };
                format!(
                    "For {secs:.1} s{sections} — {} of the time {t} is prominent at {range} — \
                     {m} sounds in the same band at {rel} {t}'s level, placed {} alike in \
                     stereo. The overlap is {pattern}. This is a potential masking conflict: \
                     {m} may make {t} less distinct there. Whether it matters is for \
                     listening to decide.",
                    pct(persistence),
                    pct(spatial),
                    t = name(target),
                    m = name(masker),
                )
            }
            (RuleId::MasterTruePeak, _) => format!(
                "The highest true peak is above the {:+.1} dBTP ceiling most delivery \
                 specifications set: encoding to a lossy format or converting may clip.",
                self.evidence_value(labels::CEILING).unwrap_or(-1.0)
            ),
            (RuleId::ClippedSamples, _) => {
                "Runs of samples sit at full scale: the waveform is likely flattened (clipped) \
                 there, unless that is the intended sound."
                    .into()
            }
            (RuleId::OverFullScale, _) => {
                "This track alone goes above 0 dBFS at the master output. Inside FaderFrame's \
                 floating-point mix nothing clips, but a fixed-point export or a device that \
                 clips will."
                    .into()
            }
            (RuleId::NonFiniteSamples, _) => {
                "The signal holds samples that are not numbers. A device on the way produced \
                 them; they were measured as silence and can make outputs fail."
                    .into()
            }
            (RuleId::SectionLoudnessStep, _) => {
                "Adjacent sections differ a lot in loudness. Often intended (a breakdown, a \
                 quiet bridge); worth a listen if not."
                    .into()
            }
            (RuleId::MonoFoldDownLoss, _) => format!(
                "Folded to mono, {range} is quieter than in stereo by the measured amount: its \
                 left and right partly cancel. Wide stereo loses some level in mono by nature; \
                 a large loss low down changes how the mix sounds on mono playback."
            ),
            (RuleId::InvertedChannel, _) => {
                "Left and right are strongly negatively correlated where both carry the \
                 signal at similar levels, as if one channel's polarity were inverted: in \
                 mono most of it cancels."
                    .into()
            }
            (RuleId::DcOffset, _) => {
                "The signal sits off zero by the measured amount. A DC offset carries no sound \
                 but takes headroom, adds up where tracks are summed and thumps where the \
                 signal starts, stops or is cut."
                    .into()
            }
            (RuleId::DeviceOverFullScale, _) => {
                let next = self
                    .evidence
                    .iter()
                    .filter_map(Evidence::device)
                    .nth(1)
                    .map_or_else(|| "the next device".into(), device);
                format!(
                    "{} sends a signal above 0 dBFS on to {next}. FaderFrame does not clip it \
                     there, but a device modelled on hardware, or working in fixed point, may \
                     — and it changes how a level-dependent device (a compressor, a \
                     saturator) reacts.",
                    dev()
                )
            }
            (RuleId::DeviceClipping, _) => format!(
                "Runs of samples at the very same level appear at {}'s output that were not \
                 at its input: it flattens peaks (clips). If it is not a clipper or limiter \
                 meant to do that, it is driven too hard.",
                dev()
            ),
            (RuleId::DeviceInvalidSamples, _) => format!(
                "{} turns valid audio into samples that are not numbers: everything after it \
                 on the way to the master is affected.",
                dev()
            ),
            (RuleId::DeviceSilences, _) => format!(
                "Audio reaches {} but nothing comes out of it over the whole range.",
                dev()
            ),
            (RuleId::DeviceNoChange, _) => format!(
                "Over the analysed range {}'s output is its input (latency aside) to within \
                 the measured difference, yet it takes the processing time measured: it may \
                 not be needed here.",
                dev()
            ),
            (RuleId::DevicesBypassed, _) => {
                "These devices are bypassed for the whole range. FaderFrame passes the audio \
                 by them, so they cost (almost) no processing; they stay loaded."
                    .into()
            }
            (RuleId::ReferenceLoudness, _) => {
                "The mix and the reference differ in loudness or dynamics by the measured \
                 amounts. A mix before mastering is usually quieter; the other comparisons are \
                 made with the reference brought to the mix's loudness."
                    .into()
            }
            (RuleId::ReferenceSpectrum, _) => {
                let d = self.evidence_value(labels::DIFFERENCE).unwrap_or(0.0);
                format!(
                    "With both at the same loudness, the mix has {:.1} dB {} at {range} than \
                     the reference over the stretch compared. A difference, not a fault: \
                     whether the mix should move towards the reference is for listening to \
                     decide.",
                    d.abs(),
                    if d >= 0.0 { "more" } else { "less" }
                )
            }
            (RuleId::ReferenceSectionBalance, _) => format!(
                "In {} the balance between the frequency regions differs from the reference's \
                 by the measured amounts, more than the other sections do (each section's \
                 balance is relative to its own loudness).",
                self.sections.first().map_or("this section", String::as_str)
            ),
            (RuleId::ReferenceStereo, _) => {
                "In the regions listed the mix's stereo image differs from the reference's: \
                 it loses more in a mono fold-down, or is wider or narrower."
                    .into()
            }
            (RuleId::PolarityInverted, Subject::Tracks { a, b }) => format!(
                "{} and {} share a signal (estimator confidence {}), and it reaches the mix \
                 the other way up in one of them: their sum holds {:+.1} dB against the sum \
                 of their energies where both play, {:+.1} dB with the polarity corrected. \
                 They may be microphones on one source, a layered sound or a copy; if so, \
                 they partly cancel each other now.",
                name(a),
                name(b),
                pct(self.evidence_value(labels::LAG_CONFIDENCE).unwrap_or(0.0)),
                self.evidence_value(labels::SUM_NOW).unwrap_or(0.0),
                self.evidence_value(labels::SUM_CORRECTED).unwrap_or(0.0),
            ),
            (RuleId::TimeOffset, Subject::Tracks { a, b }) => {
                let lag = self.evidence_value(labels::LAG).unwrap_or(0.0);
                let notch = self.evidence_value(labels::FIRST_NOTCH).unwrap_or(0.0);
                format!(
                    "{} and {} share a signal {:.2} ms apart ({} hears it later). Summed, \
                     they cancel where that delay is half a period — first around {}, then \
                     at every odd multiple — and add between: a comb filter. Their sum now \
                     holds {:+.1} dB against the sum of their energies, {:+.1} dB if the \
                     delay were taken out. The offset may be intended (a room microphone, a \
                     doubling): listen before applying.",
                    name(a),
                    name(b),
                    lag.abs(),
                    if lag >= 0.0 { name(b) } else { name(a) },
                    format_hz(notch),
                    self.evidence_value(labels::SUM_NOW).unwrap_or(0.0),
                    self.evidence_value(labels::SUM_CORRECTED).unwrap_or(0.0),
                )
            }
            (RuleId::FrequencyDependentPhase, Subject::Tracks { a, b }) => format!(
                "At {range} {} and {} share a signal (coherence {:.2}) but their phases \
                 differ by about {:.0}° there, so their sum loses {:.1} dB against the sum \
                 of their energies. Elsewhere they add as expected. No single delay or \
                 polarity explains it: a filter's phase, another microphone or cabinet, or \
                 a steady tone at that pitch.",
                name(a),
                name(b),
                self.evidence_value(labels::COHERENCE).unwrap_or(0.0),
                self.evidence_value(labels::PHASE).unwrap_or(0.0).abs(),
                -self.evidence_value(labels::SUM_NOW).unwrap_or(0.0),
            ),
            (RuleId::OnsetOffset, Subject::Tracks { a, b }) => {
                let lag = self.evidence_value(labels::ONSET_MEDIAN).unwrap_or(0.0);
                let (l, e) = if lag >= 0.0 {
                    (name(b), name(a))
                } else {
                    (name(a), name(b))
                };
                format!(
                    "Of the notes both play, {l}'s start a median {:.1} ms after {e}'s, \
                     within {:.1} ms of each other: consistently later, not random. That can \
                     be a flam, a timing feel or a delay on the way. Whether it should change \
                     is a musical decision.",
                    lag.abs(),
                    self.evidence_value(labels::ONSET_SPREAD).unwrap_or(0.0),
                )
            }
            (RuleId::LooseTiming, Subject::Tracks { a, b }) => format!(
                "{} and {} play the same rhythm, but their note starts scatter by {:.1} ms \
                 (median absolute deviation) against each other: the double may sound \
                 loose, or be meant to.",
                name(a),
                name(b),
                self.evidence_value(labels::ONSET_SPREAD).unwrap_or(0.0),
            ),
            (RuleId::WideLowEnd, _) => format!(
                "At {range} the side signal comes within {:.1} dB of the mid: the low end is \
                 wide or off-centre. Played in mono (or through a mono subwoofer) its balance \
                 changes; on a stereo system it is part of the image.",
                -self.evidence_value(labels::WIDTH).unwrap_or(0.0)
            ),
            (RuleId::MonoBalanceShift, _) => format!(
                "Folded to mono, {range} comes out {:.1} dB lower against the rest of the \
                 mix than in stereo: the parts there are wide or partly cancel. The mono mix \
                 sounds different in that range; stereo playback is not affected.",
                -self.evidence_value(labels::MONO_SHIFT).unwrap_or(0.0)
            ),
            (RuleId::TransientFlattening, _) => format!(
                "Before processing, this track's attacks stand {:.1} dB over what follows \
                 them; after it, {:.1} dB (a median over {:.0} events). Its attacks may have \
                 been flattened on the way — by compression, limiting, saturation or added \
                 sustain — possibly as intended. The device and its settings are not \
                 identified.",
                self.evidence_value(labels::TSR_SOURCE).unwrap_or(0.0),
                self.evidence_value(labels::TSR_PROCESSED).unwrap_or(0.0),
                self.evidence_value(labels::EVENTS).unwrap_or(0.0),
            ),
            (RuleId::WideLoudnessRange, _) => format!(
                "Its short-term loudness spreads over {:.1} LU (10th to 95th percentile), \
                 more than the mix's. It may come and go against the rest: quiet passages \
                 sink, loud ones stand out — or that is the arrangement.",
                self.evidence_value(labels::LRA).unwrap_or(0.0)
            ),
            (RuleId::LevelJump, _) => format!(
                "Its level changes by {:+.1} LU from one moment to the next while the mix \
                 changes by {:+.1} LU. Worth a listen for a clip gain, fade or automation \
                 step; often it is meant.",
                self.evidence_value(labels::STEP).unwrap_or(0.0),
                self.evidence_value(labels::MIX_STEP).unwrap_or(0.0),
            ),
            (RuleId::SameAudio, Subject::Tracks { a, b }) => {
                let mono = self.evidence_value(labels::MONO_SUM).unwrap_or(0.0);
                let mono = if mono >= 0.0 {
                    format!(
                        "folded to mono they add in phase, {mono:+.1} dB over the louder one \
                         alone (+6 dB for two equal copies)"
                    )
                } else {
                    format!(
                        "folded to mono they partly cancel, {mono:+.1} dB against the louder \
                         one alone (one is the other way up, or later)"
                    )
                };
                format!(
                    "For {:.0} s (the times listed) {} and {} carry the same audio, or one \
                     source processed two ways: their bands are coherent ({:.2} over {:.0} \
                     bands), which independent sounds are not, whatever their levels. Masking \
                     between them is not judged there — a copy cannot make itself less \
                     distinct. A copy adds level, not width: panned apart, the two fuse into \
                     one image between them; {mono}.",
                    self.evidence_value(labels::SAME_SECONDS).unwrap_or(0.0),
                    name(a),
                    name(b),
                    self.evidence_value(labels::COHERENCE).unwrap_or(0.0),
                    self.evidence_value(labels::SAME_BANDS).unwrap_or(0.0),
                )
            }
            _ => String::new(),
        }
    }
}

/// Evidence labels the rules use (and titles read).
pub mod labels {
    pub const CONFLICT_SECONDS: &str = "Time covered";
    pub const ACTIVE_SECONDS: &str = "Time the target is prominent in the range";
    pub const PERSISTENCE: &str = "Share of that time covered";
    pub const EVENTS: &str = "Separate events";
    pub const TARGET_LEVEL: &str = "Target band level (while covered)";
    pub const MASKER_LEVEL: &str = "Masker band level (while covered)";
    pub const MARGIN: &str = "Masker relative to target";
    pub const SPATIAL: &str = "Stereo placement alike";
    pub const SHARE: &str = "Share of the target's energy in the range";
    pub const DEPTH: &str = "Mean coverage";
    pub const TRUE_PEAK: &str = "True peak";
    pub const SAMPLE_PEAK: &str = "Sample peak";
    pub const CEILING: &str = "Ceiling";
    pub const OVERS: &str = "Overs (3+ samples at full scale)";
    pub const FULL_SCALE: &str = "Samples at full scale";
    pub const ABOVE: &str = "Samples above 0 dBFS";
    pub const NON_FINITE: &str = "Invalid samples";
    pub const INTEGRATED: &str = "Integrated loudness";
    pub const STEP: &str = "Step between sections";
    pub const FOLD_LOSS: &str = "Mono fold-down change";
    pub const CORRELATION: &str = "Correlation";
    pub const BAND_LEVEL: &str = "Band level";
    pub const CREST: &str = "Crest factor (peak to RMS)";
    pub const RMS: &str = "RMS";
    pub const LRA: &str = "Loudness range";
    pub const DC: &str = "DC offset (largest channel)";
    pub const DEVICE: &str = "Device";
    pub const NEXT_DEVICE: &str = "Next device";
    pub const OUT_PEAK: &str = "Peak at its output";
    pub const IN_PEAK: &str = "Peak at its input";
    pub const OUT_ABOVE: &str = "Samples above 0 dBFS at its output";
    pub const OUT_FLAT: &str = "Flattened runs at its output";
    pub const IN_FLAT: &str = "Flattened runs at its input";
    pub const OUT_INVALID: &str = "Invalid samples at its output";
    pub const IN_RMS: &str = "RMS at its input";
    pub const OUT_RMS: &str = "RMS at its output";
    pub const CHANGE: &str = "What it changes, relative to its input";
    pub const COST: &str = "Processing time (share of real time)";
    pub const MIX_LUFS: &str = "Mix integrated loudness";
    pub const REFERENCE_LUFS: &str = "Reference integrated loudness";
    pub const LOUDNESS_DIFFERENCE: &str = "Mix minus reference (loudness)";
    pub const REFERENCE_LRA: &str = "Reference loudness range";
    pub const MIX_PLR: &str = "Mix peak to loudness";
    pub const REFERENCE_PLR: &str = "Reference peak to loudness";
    pub const DIFFERENCE: &str = "Mix minus reference (mean, loudness-matched)";
    pub const LARGEST_DIFFERENCE: &str = "Largest difference";
    pub const SONG_DIFFERENCE: &str = "The sections' typical difference there";
    pub const REFERENCE_SECONDS: &str = "Reference compared over";
    // Phase and timing (positive lags: the second track later).
    pub const LAG: &str = "Delay of the second track";
    pub const LAG_SAMPLES: &str = "Delay in samples";
    pub const LAG_CONFIDENCE: &str = "Estimator confidence";
    pub const CLARITY: &str = "Peak clarity";
    pub const POLARITY: &str = "Correlation peak's sign";
    pub const SUM_NOW: &str = "Sum against energies now";
    pub const SUM_CORRECTED: &str = "Sum against energies corrected";
    pub const SUM_MONO: &str = "Sum against energies in mono";
    pub const SUM_FLIPPED: &str = "Sum with the polarity flipped";
    pub const GAIN: &str = "Brought back by the correction";
    pub const FIRST_NOTCH: &str = "First cancellation";
    pub const NOTCH_SPACING: &str = "Cancellations every";
    pub const WORST_BAND: &str = "Most cancelled band";
    pub const WORST_BAND_SUM: &str = "Its sum against energies";
    pub const COHERENCE: &str = "Coherence";
    pub const PHASE: &str = "Phase difference";
    pub const TOGETHER: &str = "Compared over";
    pub const ONSETS_MATCHED: &str = "Onsets paired";
    pub const ONSET_SHARE: &str = "Share of onsets paired";
    pub const ONSET_MEDIAN: &str = "Median onset offset";
    pub const ONSET_SPREAD: &str = "Onset spread (median absolute deviation)";
    // Width and dynamics.
    pub const WIDTH: &str = "Side against mid";
    pub const MONO_CHANGE: &str = "Mono fold-down change";
    pub const MONO_SHIFT: &str = "Mono change against the whole";
    pub const SIDE_SHARE: &str = "Share of the side signal there";
    pub const TSR_SOURCE: &str = "Transient-to-sustain before processing";
    pub const TSR_PROCESSED: &str = "Transient-to-sustain after processing";
    pub const TSR_CHANGE: &str = "Transient-to-sustain change";
    pub const ATTACK_CHANGE: &str = "Attack rise change";
    pub const CREST_SOURCE: &str = "Crest factor before processing";
    pub const CREST_CHANGE: &str = "Crest factor change";
    pub const PLR: &str = "Peak to loudness ratio";
    pub const SHORT_TERM_LOW: &str = "Short-term loudness, 10th percentile";
    pub const SHORT_TERM_MID: &str = "Short-term loudness, median";
    pub const SHORT_TERM_HIGH: &str = "Short-term loudness, 95th percentile";
    pub const ACTIVE: &str = "Active time";
    pub const MIX_LRA: &str = "The mix's loudness range";
    pub const MIX_STEP: &str = "The mix's change at the same time";
    pub const BEFORE: &str = "Momentary loudness before";
    pub const AFTER: &str = "Momentary loudness after";
    pub const PAIR_SHARE: &str = "Share of the pair's energy there";
    pub const WIDE_SECONDS: &str = "Time that wide";
    pub const WHOLE_MONO: &str = "Mono change of the whole";
    // The same audio.
    pub const SAME_SECONDS: &str = "Time the same audio";
    pub const SAME_BANDS: &str = "Bands judged (mean)";
    pub const BAND_COHERENCE: &str = "Coherence, plain mean over bands";
    pub const LEVEL_BETWEEN: &str = "Level of the second (mono fold-downs)";
    pub const MONO_SUM: &str = "Mono sum against the louder alone";
    pub const SAME_LAG: &str = "Delay between them";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn experiments_are_validated() {
        let t = TrackId(1);
        let ok = Experiment::EqCut {
            track: t,
            freq_hz: 80.0,
            gain_db: -3.0,
            q: 1.4,
        };
        assert!(ok.validate(24_000.0).is_ok());
        let boost = Experiment::EqCut {
            track: t,
            freq_hz: 80.0,
            gain_db: 3.0,
            q: 1.4,
        };
        assert!(boost.validate(24_000.0).is_err(), "cuts only");
        let deep = Experiment::EqCut {
            track: t,
            freq_hz: 80.0,
            gain_db: -12.0,
            q: 1.4,
        };
        assert!(deep.validate(24_000.0).is_err());
        let high = Experiment::EqCut {
            track: t,
            freq_hz: 30_000.0,
            gain_db: -2.0,
            q: 1.4,
        };
        assert!(high.validate(24_000.0).is_err());
        let own = Experiment::DynamicEqCut {
            track: t,
            key: t,
            freq_hz: 80.0,
            range_db: -3.0,
            q: 1.0,
        };
        assert!(own.validate(24_000.0).is_err());
        let nan = Experiment::Level {
            track: t,
            change_db: f64::NAN,
        };
        assert!(nan.validate(24_000.0).is_err());
        assert!(
            Experiment::Manual { text: "x".into() }
                .validate(24_000.0)
                .is_ok()
        );
        let name = |id: TrackId| format!("T{}", id.0);
        assert_eq!(ok.title(&name), "EQ cut on T1: -3.0 dB at 80 Hz (Q 1.4)");
    }

    #[test]
    fn evidence_rounds_and_never_holds_nan() {
        let e = Evidence::new("x", f64::NAN, Unit::Db, None);
        assert_eq!(e.value, 0.0);
        assert_eq!(
            Evidence::new("x", 0.4567, Unit::Share, None).value_text(),
            "46 %"
        );
        assert_eq!(
            Evidence::new("x", -3.04, Unit::Db, None).value_text(),
            "-3.0 dB"
        );
        assert_eq!(Grade::of_severity(0.6), Grade::High);
        assert_eq!(Grade::of_confidence(0.6), Grade::Moderate);
    }
}
