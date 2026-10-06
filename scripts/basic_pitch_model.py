#!/usr/bin/env python3
"""Convert basic-pitch's model (Spotify, Apache-2.0) for faderframe-transcribe.

    uv run --no-project --python 3.12 --with onnx --with onnxruntime --with numpy \
        python scripts/basic_pitch_model.py <basic-pitch checkout>

writes crates/faderframe-transcribe/model/basic-pitch.ffnn (the graph of
basic_pitch/saved_models/icassp_2022/nmp.onnx: a JSON header — inputs,
outputs, tensors, nodes with attributes — then the tensors' little-endian
data) and tests/reference.json (onnxruntime's outputs for the test signal
the Rust tests make: sums along each axis and sampled values).
"""
import json, math, struct, sys
from pathlib import Path

import numpy as np
import onnx
from onnx import numpy_helper

ROOT = Path(__file__).resolve().parent.parent
CRATE = ROOT / "crates" / "faderframe-transcribe"
DTYPES = {np.dtype("float32"): ("f32", "<f4"), np.dtype("int64"): ("i64", "<i8"),
          np.dtype("int32"): ("i64", "<i8")}


def attr(a):
    v = onnx.helper.get_attribute_value(a)
    if isinstance(v, bytes):
        return v.decode()
    if isinstance(v, (list, tuple)):
        return [x.decode() if isinstance(x, bytes) else x for x in v]
    if isinstance(v, onnx.TensorProto):
        raise SystemExit(f"tensor attribute {a.name} not supported")
    return v


def convert(model_path: Path):
    m = onnx.load(str(model_path))
    g = m.graph
    blobs, tensors, offset = [], [], 0
    for init in g.initializer:
        arr = numpy_helper.to_array(init)
        kind, fmt = DTYPES[arr.dtype]
        data = arr.astype(fmt).tobytes()
        tensors.append({"name": init.name, "dtype": kind, "shape": list(arr.shape),
                        "offset": offset, "len": len(data)})
        blobs.append(data)
        offset += len(data)
    nodes = [{"op": n.op_type, "inputs": list(n.input), "outputs": list(n.output),
              "attrs": {a.name: attr(a) for a in n.attribute}} for n in g.node]
    header = {"format": 1, "source": "basic-pitch icassp_2022 nmp.onnx (Apache-2.0)",
              "inputs": [i.name for i in g.input if i.name not in {t["name"] for t in tensors}],
              "outputs": [o.name for o in g.output], "tensors": tensors, "nodes": nodes}
    head = json.dumps(header, separators=(",", ":")).encode()
    out = CRATE / "model" / "basic-pitch.ffnn"
    with open(out, "wb") as f:
        f.write(b"FFNN1\n")
        f.write(struct.pack("<I", len(head)))
        f.write(head)
        for b in blobs:
            f.write(b)
    print(out, out.stat().st_size, "bytes,", len(nodes), "nodes,", len(tensors), "tensors")
    return header


def test_signal():
    # As tests/model.rs makes it: C4 E4 G4 for a second, then A3, harmonic
    # tones at 22050 Hz, 43844 samples.
    n = 43844
    out = np.zeros(n, dtype=np.float64)
    for i in range(n):
        t = i / 22050.0
        notes = [60, 64, 67] if t < 1.0 else [57]
        v = 0.0
        for k in notes:
            f = 440.0 * 2.0 ** ((k - 69) / 12.0)
            for h in range(1, 6):
                v += math.sin(2 * math.pi * f * h * t) / h
        out[i] = 0.1 * v
    return out.astype(np.float32)


def reference(model_path: Path, header):
    import onnxruntime as ort
    s = ort.InferenceSession(str(model_path))
    x = test_signal().reshape(1, -1, 1)
    outs = s.run(None, {header["inputs"][0]: x})
    names = [o.name for o in s.get_outputs()]
    ref = {}
    for name, arr in zip(names, outs):
        a = arr[0].astype(np.float64)
        rng = np.random.default_rng(7)
        idx = rng.integers(0, a.size, 300)
        ref[name] = {"shape": list(a.shape), "frame_sums": a.sum(axis=1).tolist(),
                     "bin_sums": a.sum(axis=0).tolist(),
                     "samples": [[int(i), float(a.flat[i])] for i in idx]}
    path = CRATE / "tests" / "reference.json"
    path.write_text(json.dumps(ref))
    print(path, path.stat().st_size, "bytes")


if __name__ == "__main__":
    src = Path(sys.argv[1]) / "basic_pitch" / "saved_models" / "icassp_2022" / "nmp.onnx"
    h = convert(src)
    reference(src, h)
