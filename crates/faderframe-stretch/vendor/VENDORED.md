# Vendored code

| Directory | Upstream | Version | Licence |
|---|---|---|---|
| `signalsmith-stretch/` | https://github.com/Signalsmith-Audio/signalsmith-stretch | 1.3.1 | MIT (`signalsmith-stretch/LICENSE.txt`) |
| `signalsmith-linear/` | https://github.com/Signalsmith-Audio/linear | as shipped with Stretch 1.3.1 | MIT (`signalsmith-linear/LICENSE.txt`) |

Taken from the `signalsmith-stretch` 0.1.3 crate's bundled sources (headers
only; tests, CMake files, the web demo and command-line tool left out).

## Local changes

- `signalsmith-stretch.h`, `configure()`: `peaks.reserve(bands)` instead of
  `bands/2` — `findPeaks()` can find up to `(bands + 1)/2` peaks, and the
  vector must never grow while processing (realtime safety; see
  `tests/stretch.rs`, which counts C++ allocations).
