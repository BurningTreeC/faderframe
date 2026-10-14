# GainStageFx DSP extraction

Source: ../DAW_PLUGINS/gainstagefx, revision `436f04141eb343ac757ef255b165f58f60858022`, version 0.49.0.
MIT OR Apache-2.0; original licence texts are included.

The numerical DSP modules and six microphone circuit descriptions are copied
from that snapshot. Neve's optional loudspeaker adapter is omitted; its line
output stage is retained. No plugin framework or GUI dependencies are included.
Upstream application-specific test code is retained as reference with its `cfg(test)`
changed to `cfg(any())`; it depends on the original application catalogue.
FaderFrame integration tests exercise the extracted library directly.

`preamp` adapts the circuits to calibrated digital audio. Future circuit plugins
can use `dsp::netlist`, `dsp::time`, `dsp::ac`, and `dsp::oversample` directly.
The Tube 610 uses fixed output calibration at the default 50% setting: its
Gain control moves the physical interstage Level pot. Drive-dependent makeup
would cancel this attenuation and amplify capacitive feedthrough at zero.
The host also retains GainStageFx's exact duplicated-mono optimization: one
circuit runs until stereo input diverges, then its solver and resampler state
are copied into the dormant channel without allocating. Both channels remain
active thereafter, preserving their independent tails until reset.

GainStageFx's generic `reservoir.rs`, its unit tests and test allocator are
also retained in `faderframe-realtime`. Local adaptations use FaderFrame's
portable scheduling/denormal helpers, support up to 64 channels, name the
thread `faderframe-circuit`, and return thread creation errors. The kernel
generator (`examples/kernels.rs`, not part of the snapshot) is rewritten
here: it regenerates upstream's 218 kernels byte for byte and adds those the
preamps and console buses need (a census through their Gain at three rates
on six signals; 65 at the time of writing), and `KERNEL_CANDIDATES` is 64.
The numerical solver is unchanged apart from the test configuration
described above and three local changes:

- `partition`: the Schur recovery of the internal unknowns and the per-sample
  RHS responses walk their matrices by row without the exact zeros
  (`SparseRows`; 80-90 % of the entries on the preamps), each sum still taken
  in the original order: outputs are bit-identical (to the sign of an exact
  zero) for every circuit, guitar voices included.
- `time`: `Simulation::set_early_acceptance` (off by default; the preamps and
  console buses turn it on) stops a solve after its first Newton pass when
  the quadratic constant measured on recent two-pass solves predicts the
  second correction at a hundredth of the tolerance. -117 dB relative to the
  signal at worst on every preamp and bus; see its documentation.
- `device`: `Device::crossed`, which that test uses to keep a triode's or
  pentode's corner (grid conduction, plate or screen edge) out of it, and
  `Device::junction`, which narrows it for circuits with PN junctions.

With early acceptance on, the reduced line search also keeps the trial
evaluations an accepted trial point's stamp reuses (upstream's
accepted-trial cache, so far only the Twin's; bit-identical here too).

Reservoir test adaptations account for input-overflow concealment and delay
re-priming, and use a gated worker tail to verify partial publication without
depending on sub-millisecond scheduler timing on hosted runners.

The reservoir also accepts an explicit device callback deadline. Live graphs
with buffered preamps request at most 128 frames per internal chunk, so every
track can enqueue work before the next chunk needs its output. All chunks
share the device callback's remaining budget, with 10% (at least 100 us)
reserved for downstream work; they do not infer separate deadlines from pace.

Live preamps use that worker with a reservoir of one device callback (at
least 128 samples), in addition to FIR latency, and give the solver the
audio's due time as its realtime deadline (`Preamp::set_realtime_deadline`).
A track's channels are solved in parallel by the worker and helper threads,
a thread per channel (a 5.1 track six) within the physical cores and the
user's processing threads (`faderframe_realtime::channel_threads`).
Automation travels per input sample in two extra reservoir lanes, keeping
host callback sizes and structural wait budgets intact.
Offline/ahead graphs instead process inline with the same delay and no
deadline; the host reports the same latency in both modes. Underruns are included in
the engine's xrun counter. Layouts above the reservoir's channel limit retain
the synchronous path and identical latency.

Reproducible release measurements (run without concurrent builds/tests):

```sh
cargo test --release -p faderframe-plugin-host duplicated_mono_throughput -- --ignored --nocapture
cargo run --release -p faderframe-bench -- --tracks 8 --threads 4 --block 128 --seconds 10 --buses 0 --no-inserts --no-sends --preamp 0 --paced
```

Omit `--paced` to measure the complete synchronous DSP cost. Add `--mono-source`
for a mono tone routed into stereo tracks, or `--ahead 200` to measure normal
render-ahead playback. The worker changes where DSP runs, not its total cost;
buffers larger than the reservoir still need some work from the current call.
