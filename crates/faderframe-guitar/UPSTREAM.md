# GainStageFx guitar extraction

Source: ../DAW_PLUGINS/gainstagefx (GainStageFx by Simon Huber), revision
`436f04141eb343ac757ef255b165f58f60858022`, version 0.49.0. MIT OR Apache-2.0;
the original licence texts are included. The owner asked for these parts to
become FaderFrame's built-in Guitar Station (2026-10-07).

## Taken unchanged

- `src/circuits/`: every guitar and bass amplifier, power stage
  (`power.rs`, `jc120_power.rs`, `american_ss800.rs`), pedal, wah, the legacy
  cabinet filter and tone stack, and the helpers they share (`valve`, `jfet`,
  `rectifier`, `iron`, `clipper`, `preamp`, `studio`). `neve.rs` is upstream's
  own (its loudspeaker adapter is named by the power-stage table); the other
  five microphone preamplifiers are `faderframe-circuit`'s. Two Schur-core
  tests print without `nonlinear_boundary_names`, a solver method that exists
  only in upstream's test build.
- `src/acoustics/`: loudspeaker, enclosure, diffraction, radiation, cabinet
  and microphone models (`SPEAKER_MODEL.md`, `CABINET_MODEL.md`,
  `MICROPHONE_MODEL.md` upstream).
- `src/dsp/{bbd,spring,tremolo}.rs`: the bucket brigade (plus a
  `Brigade::copy_runtime_state_from` for waking a dormant channel), the
  spring tank and the optical tremolo.
- `calibration.rs`, `power_trim.rs`, `direct_out.rs`: the measured tables.

The solver is FaderFrame's (`faderframe-circuit::dsp`, re-exported as
`dsp`), which is upstream's numerical code with its test-only paths compiled
out.

## Adapted

- `voice.rs` keeps upstream's catalogue -- `Gain`, `Pedal`, `PowerModel`,
  `PowerAmp`, the acoustic settings, the builders, calibration, `Delay` --
  and drops `Chain`, its pipelining, speculation and stage worker (all of
  upstream's `unsafe`). `master_position`/`master_lift` became free functions.
- `chain.rs` is upstream's `Chain` for the amplifier and everything after it:
  the preamplifier with its reverb, tremolo, chorus and graphic, the power
  stage into a resistor or the loudspeaker, horn, cabinet and microphones,
  the switch fade, and the DI taps (input, pedals, preamplifier/direct out).
  Without upstream's pedal slot, wah, output iron, microphone preamplifiers,
  plugin tone stack and dry/wet handling. It builds only the amplifiers
  the Guitar Station offers (`AMPS`) and their power stages; the oversampling
  is fixed per chain, so the latency never moves.
- `pedal.rs`: each pedal or wah is a stage of its own (`PedalStage`), at its
  own oversampling (an expensive pedal at the host rate), padded to a fixed
  latency, behind a true-bypass footswitch. Levels are upstream's: a pedal
  is handed `Pedal::input_volts`, a wah `GUITAR_VOLTS`, and the signal between
  stages is in the line's digital units, so a pedal into the amplifier is
  upstream's `pedal_hand_off` and pedals in series hand each other volts as a
  cable would. Circuits are built off the audio thread (`StompCircuit`).
- The variac (`mains`) reaches the amplifier only; the pedals keep their
  batteries.

- The noise gate (`noise_gate`) is upstream's `dsp/noise_reduction.rs`
  unchanged (renamed `NoiseGate`): the input expander after the Input trim,
  one detector on the hotter channel, threshold -90…-30 dBFS.

## Added here

Not upstream's, written for FaderFrame: the compressors `circuits::red_squeeze`
(MXR Dyna Comp, CA3080 version, with the Ross Compressor's values as
`red_squeeze::ROSS`) and `circuits::juicer` (Dan Armstrong Orange Squeezer),
`Pedal::{RedSqueeze, Juicer}` on the line only (no `Gain` of their own), and
the CA3080 the Red Squeeze is built round (`faderframe_circuit`'s
`device::Ota`, `netlist::OtaSpec::CA3080`).

## Checked

A scratch harness built upstream's untouched `voice.rs` on these modules and
compared it with `PedalStage` + `Chain` at 1x over 0.5 s of plucked notes:
all 20 amplifiers bit-identical, on the legacy path and through the physical
speaker, cabinet and a stereo pair of microphones; every pedal and both wahs
in front of an amplifier within 1e-8 of the peak (the hand-off's
multiplication order).

A full set of pedal circuits is about 5.8 MB per channel (the Bass Driver
alone 1.1 MB), so a place on the line holds only its pedal's circuit, built
when it is chosen (about 1-2 ms).

## Added in FaderFrame

Circuits written for FaderFrame after the extraction, in upstream's manner
(component-level netlists on the same solver, every part value against its
reference designator, sources cited in each module) but not upstream's:

- `circuits/tweed_lux.rs`, `power::PowerSpec::TWEED_6V6` with
  `power::CathodyneFront` and `power::T5Y3GT`: the Fender Deluxe 5E3, both
  channels (`Gain::TweedLux`, `Gain::TweedLuxNormal`).
- `circuits/seattle100.rs`, `power::PowerSpec::SEATTLE_6L6` with
  `power::DepthNetwork`: the Soldano SLO-100, both channels
  (`Gain::Seattle100`, `Gain::Seattle100Normal`).

Their rows in `calibration.rs` and `power_trim.rs` are measured by
`tests/calibrate_new_amps.rs`, which follows upstream's procedure and anchor
(it re-measures the JTM45's committed row to the printed digit before it
prints anything). Upstream's rows are untouched. The two power stages are
matched only: neither is a `PowerAmp` choice, so `POWER_TRIM_DB` gained rows
but no column.
