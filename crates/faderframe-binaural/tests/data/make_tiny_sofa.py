"""Write tiny.sofa: a minimal SimpleFreeFieldHRIR file for the SOFA reader's
test. Six directions (front, left, right, back, up, down); each left ear an
impulse at sample 0, each right ear half as loud one sample later; a delay
of 3 samples on both ears; 64 taps at 48 kHz.

    uv run --no-project --with h5py --with numpy python make_tiny_sofa.py
"""

from pathlib import Path

import h5py
import numpy as np

here = Path(__file__).parent
dirs = [(0, 0), (90, 0), (270, 0), (180, 0), (0, 90), (0, -90)]
m, n = len(dirs), 64
ir = np.zeros((m, 2, n))
ir[:, 0, 0] = 1.0
ir[:, 1, 1] = 0.5
with h5py.File(here / "tiny.sofa", "w") as f:
    f.attrs["Conventions"] = "SOFA"
    f.attrs["SOFAConventions"] = "SimpleFreeFieldHRIR"
    f.attrs["DataType"] = "FIR"
    f.attrs["Title"] = ""
    f["Data.IR"] = ir
    f["Data.SamplingRate"] = np.array([48000.0])
    f["Data.Delay"] = np.array([[3.0, 3.0]])
    pos = f.create_dataset("SourcePosition", data=np.array([[a, e, 1.2] for a, e in dirs], dtype=float))
    pos.attrs["Type"] = "spherical"
    pos.attrs["Units"] = "degree, degree, metre"
print("wrote", here / "tiny.sofa")
