"""Bake the HRIRs FaderFrame's binaural monitoring uses.

From the SADIE II database's KU100 dummy head (D1; University of York,
Apache-2.0: https://zenodo.org/records/10886409), for each direction a
speaker of FaderFrame's surround formats stands in: the nearest measured
direction, both ears trimmed by their common onset (the interaural delay
stays in), cut to a multiple of 64 taps (128 at 96 kHz) with a short fade,
and all normalised so the front centre is at unity at 1 kHz.

Run with the D1 SOFA files unpacked (D1_HRIR_SOFA.zip from the record):

    uv run --no-project --with h5py --with numpy python scripts/binaural_hrirs.py \
        <dir with D1_44K_16bit_256tap_FIR_SOFA.sofa, D1_48K_…, D1_96K_…> \
        crates/faderframe-binaural/data/sadie-d1.ffhr

Output ("FFHR" v1, little endian): u32 magic, u32 version, u32 sets; per
set: u32 rate, u32 taps, u32 directions; per direction: f32 azimuth
(degrees, positive to the left), f32 elevation, taps × f32 left, taps × f32
right.
"""

import struct
import sys
from pathlib import Path

import h5py
import numpy as np

# (azimuth positive left, elevation) for every speaker direction FaderFrame
# uses (ITU-R BS.2051 angles; the tops a little higher for the side tops).
DIRECTIONS = [
    (0, 0),  # C, mono
    (30, 0), (-30, 0),  # L, R
    (90, 0), (-90, 0),  # Lss, Rss
    (110, 0), (-110, 0),  # Ls, Rs (5.x)
    (135, 0), (-135, 0),  # Lrs, Rrs; quad's rear pair
    (45, 30), (-45, 30),  # Ltf, Rtf
    (135, 30), (-135, 30),  # Ltr, Rtr
    (90, 45), (-90, 45),  # Ltm, Rtm
]

FILES = [
    (44_100, "D1_44K_16bit_256tap_FIR_SOFA.sofa", 64),
    (48_000, "D1_48K_24bit_256tap_FIR_SOFA.sofa", 64),
    (96_000, "D1_96K_24bit_512tap_FIR_SOFA.sofa", 128),
]


def unit(az, el):
    a, e = np.radians(az), np.radians(el)
    return np.array([np.cos(e) * np.cos(a), np.cos(e) * np.sin(a), np.sin(e)])


def bake(path, block):
    h = h5py.File(path, "r")
    ir = np.array(h["Data.IR"])  # (M, 2, N)
    pos = np.array(h["SourcePosition"])  # az 0…359 (counter-clockwise), el, r
    rate = float(np.array(h["Data.SamplingRate"])[0])
    vecs = np.stack([unit(a, e) for a, e, _ in pos])
    out = []
    for az, el in DIRECTIONS:
        i = int(np.argmax(vecs @ unit(az % 360, el)))
        pair = ir[i]
        peak = np.abs(pair).max()
        onset = min(int(np.argmax(np.abs(ch) > peak * 0.01)) for ch in pair)
        start = max(onset - 8, 0)
        cut = pair[:, start:]
        taps = (cut.shape[1] // block) * block
        cut = cut[:, :taps].copy()
        fade = min(16, taps)
        cut[:, taps - fade:] *= np.cos(np.linspace(0, np.pi / 2, fade)) ** 2
        out.append((az, el, cut))
    # The front centre at unity at 1 kHz (both ears' mean).
    c = out[0][2]
    n = 1 << 14
    spec = np.abs(np.fft.rfft(c, n, axis=1))
    k = int(round(1000.0 / rate * n))
    norm = float(spec[:, k].mean())
    taps = max(d[2].shape[1] for d in out)
    padded = []
    for az, el, cut in out:
        p = np.zeros((2, taps))
        p[:, : cut.shape[1]] = cut / norm
        padded.append((az, el, p))
    return int(rate), taps, padded


def main():
    src, dst = Path(sys.argv[1]), Path(sys.argv[2])
    sets = [bake(src / name, block) for _, name, block in FILES]
    with open(dst, "wb") as f:
        f.write(struct.pack("<4sII", b"FFHR", 1, len(sets)))
        for rate, taps, dirs in sets:
            f.write(struct.pack("<III", rate, taps, len(dirs)))
            for az, el, p in dirs:
                f.write(struct.pack("<ff", az, el))
                f.write(p[0].astype("<f4").tobytes())
                f.write(p[1].astype("<f4").tobytes())
            print(f"{rate} Hz: {len(dirs)} directions × {taps} taps")
    print(f"wrote {dst} ({dst.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
