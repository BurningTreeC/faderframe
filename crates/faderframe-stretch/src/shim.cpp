// C interface to Signalsmith Stretch for faderframe-stretch (non-interleaved
// float channels). Configuration allocates; everything else must not.
#include "signalsmith-stretch/signalsmith-stretch.h"

#include <cstdint>

using Stretch = signalsmith::stretch::SignalsmithStretch<float>;

extern "C" {

void *ff_stretch_new(int32_t channels, int32_t block, int32_t interval, uint32_t seed) {
    try {
        auto *s = new Stretch(long(seed));
        s->configure(channels, block, interval, false);
        return s;
    } catch (...) {
        return nullptr;
    }
}

void ff_stretch_free(void *s) { delete static_cast<Stretch *>(s); }

void ff_stretch_reset(void *s) { static_cast<Stretch *>(s)->reset(); }

int32_t ff_stretch_input_latency(const void *s) {
    return static_cast<const Stretch *>(s)->inputLatency();
}

int32_t ff_stretch_output_latency(const void *s) {
    return static_cast<const Stretch *>(s)->outputLatency();
}

int32_t ff_stretch_block(const void *s) { return static_cast<const Stretch *>(s)->blockSamples(); }

int32_t ff_stretch_interval(const void *s) {
    return static_cast<const Stretch *>(s)->intervalSamples();
}

void ff_stretch_seek(void *s, const float *const *inputs, int32_t length, double rate) {
    static_cast<Stretch *>(s)->seek(inputs, length, rate);
}

void ff_stretch_process(void *s, const float *const *inputs, int32_t input_length,
                        float *const *outputs, int32_t output_length) {
    static_cast<Stretch *>(s)->process(inputs, input_length, outputs, output_length);
}

void ff_stretch_set_transpose(void *s, float factor) {
    static_cast<Stretch *>(s)->setTransposeFactor(factor);
}

}
