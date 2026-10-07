"""Bake the HRIRs FaderFrame's binaural monitoring uses.

From the SADIE II database (University of York, Apache-2.0:
https://zenodo.org/records/10886409) — the KU100 (D1) and KEMAR (D2) dummy
heads and eighteen listeners (H3…H20) — for each direction a speaker of
FaderFrame's surround formats stands in: the nearest measured direction,
both ears trimmed by their common onset (the interaural delay stays in),
cut to a multiple of 64 taps (128 at 96 kHz) with a short fade, and all
normalised so the front centre is at unity at 1 kHz (heads compare at the
same level).

Run with the record's <S>_HRIR_SOFA.zip archives unpacked side by side
(each into <S>_HRIR_SOFA/):

    uv run --no-project --with h5py --with numpy python scripts/binaural_hrirs.py \
        <dir of the unpacked archives> crates/faderframe-binaural/data [D1 D2 H3 …]

It writes data/sadie-<s>.ffhr per subject (all twenty by default) and
prints how far the farthest direction is from the one asked for.

The same code reads any SOFA file (SimpleFreeFieldHRIR) at run time:
`faderframe_binaural::sofa`.

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
    (44_100, "{s}_44K_16bit_256tap_FIR_SOFA.sofa", 64),
    (48_000, "{s}_48K_24bit_256tap_FIR_SOFA.sofa", 64),
    (96_000, "{s}_96K_24bit_512tap_FIR_SOFA.sofa", 128),
]

SUBJECTS = ["D1", "D2"] + [f"H{n}" for n in range(3, 21)]


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
    worst = 0.0
    for az, el in DIRECTIONS:
        i = int(np.argmax(vecs @ unit(az % 360, el)))
        off = np.degrees(np.arccos(np.clip(vecs[i] @ unit(az % 360, el), -1, 1)))
        worst = max(worst, float(off))
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
    return int(rate), taps, padded, worst


def write(dst, sets):
    with open(dst, "wb") as f:
        f.write(struct.pack("<4sII", b"FFHR", 1, len(sets)))
        for rate, taps, dirs, _ in sets:
            f.write(struct.pack("<III", rate, taps, len(dirs)))
            for az, el, p in dirs:
                f.write(struct.pack("<ff", az, el))
                f.write(p[0].astype("<f4").tobytes())
                f.write(p[1].astype("<f4").tobytes())


def main():
    src, out = Path(sys.argv[1]), Path(sys.argv[2])
    for subject in sys.argv[3:] or SUBJECTS:
        folder = src / f"{subject}_HRIR_SOFA"
        files = list(folder.rglob(FILES[0][1].format(s=subject)))
        base = files[0].parent if files else folder
        sets = [bake(base / name.format(s=subject), block) for _, name, block in FILES]
        dst = out / f"sadie-{subject.lower()}.ffhr"
        write(dst, sets)
        taps = "/".join(str(t) for _, t, _, _ in sets)
        worst = max(w for *_, w in sets)
        print(f"{subject}: {taps} taps, farthest direction {worst:.1f}° off, {dst.stat().st_size} bytes")


if __name__ == "__main__":
    main()
