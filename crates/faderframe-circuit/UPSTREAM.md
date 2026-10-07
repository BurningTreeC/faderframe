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
thread `faderframe-circuit`, and return thread creation errors. The numerical
solver and generated kernels are unchanged apart from the test configuration
described above.

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
A track's channels are solved in parallel by the worker and helper threads.
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
