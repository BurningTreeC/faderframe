#!/usr/bin/env python3
"""Reference outputs for faderframe-separate (stem separation, X-UMX).

    uv run --no-project --python 3.12 \
        --index https://download.pytorch.org/whl/cpu --index-strategy unsafe-best-match \
        --with torch --with numpy --with scipy --with norbert \
        python -I scripts/separation_reference.py [--checkpoint pretrained_xumxl.pth]

Without a checkpoint it makes a tiny X-UMX with random weights (fixed
seed), saves it as asteroid saves a model (`torch.save` of `model_name`,
`state_dict`, `model_args`, `infos`) into
crates/faderframe-separate/tests/data/tiny-xumx.pth, and writes what it
gives for a short test signal to tests/reference-tiny.json. With the
published X-UMXL checkpoint (Sony, CC BY 4.0, doi:10.5281/zenodo.7128659;
never committed) it writes tests/reference-xumxl.json for the opt-in test.

The separation is asteroid's X-UMX recipe (egs/musdb18/X-UMX/eval.py):
the network's masked magnitudes, norbert's multichannel Wiener filter with
the mixture phase as the first estimate (`softmask=False`), then scipy's
inverse STFT. The model definition below is asteroid's
(asteroid/models/x_umx.py, MIT, Copyright (c) 2019 Pariente Manuel), cut
to inference, with the same module names so the checkpoints load as they
are. The test signal is the one the Rust tests make (`tests/common`):
change both together.
"""
import argparse
import json
import math
from pathlib import Path

import norbert
import numpy as np
import scipy.signal
import torch
import torch.nn as nn
import torch.nn.functional as F

ROOT = Path(__file__).resolve().parent.parent
CRATE = ROOT / "crates" / "faderframe-separate"
SOURCES = ["bass", "drums", "vocals", "other"]


class _STFT(nn.Module):
    def __init__(self, window_length, n_fft, n_hop):
        super().__init__()
        self.window = nn.Parameter(torch.hann_window(window_length), requires_grad=False)
        self.n_fft = n_fft
        self.n_hop = n_hop

    def forward(self, x):
        nb_samples, nb_channels, _ = x.size()
        x = x.reshape(nb_samples * nb_channels, -1)
        stft_f = torch.stft(x, n_fft=self.n_fft, hop_length=self.n_hop, window=self.window,
                            center=True, normalized=False, onesided=True, pad_mode="reflect",
                            return_complex=True)
        stft_f = torch.view_as_real(stft_f)
        return stft_f.contiguous().view(nb_samples, nb_channels, self.n_fft // 2 + 1, -1, 2)


class _Spectrogram(nn.Module):
    def forward(self, stft_f):
        phase = stft_f.detach().clone()
        phase = torch.atan2(phase[..., 1], phase[..., 0])
        stft_f = stft_f.transpose(2, 3)
        stft_f = stft_f.pow(2).sum(-1).pow(0.5)
        return [stft_f.permute(2, 0, 1, 3), phase]


class _Enc(nn.Module):
    def __init__(self, nb_bins, hidden_size, nb_channels):
        super().__init__()
        self.max_bin = nb_bins
        self.hidden_size = hidden_size
        self.enc = nn.Sequential(nn.Linear(nb_bins * nb_channels, hidden_size, bias=False),
                                 nn.BatchNorm1d(hidden_size))

    def forward(self, x, shapes):
        nb_frames, nb_samples, nb_channels, _ = shapes
        x = self.enc(x.reshape(-1, nb_channels * self.max_bin))
        return torch.tanh(x.reshape(nb_frames, nb_samples, self.hidden_size))


class _Dec(nn.Module):
    def __init__(self, nb_output_bins, hidden_size, nb_channels):
        super().__init__()
        self.nb_output_bins = nb_output_bins
        self.dec = nn.Sequential(
            nn.Linear(hidden_size * 2, hidden_size, bias=False),
            nn.BatchNorm1d(hidden_size),
            nn.ReLU(),
            nn.Linear(hidden_size, nb_output_bins * nb_channels, bias=False),
            nn.BatchNorm1d(nb_output_bins * nb_channels),
        )

    def forward(self, x, shapes):
        nb_frames, nb_samples, nb_channels, _ = shapes
        x = self.dec(x.reshape(-1, x.shape[-1]))
        return x.reshape(nb_frames, nb_samples, nb_channels, self.nb_output_bins)


class XUMX(nn.Module):
    def __init__(self, sources, window_length=4096, in_chan=4096, n_hop=1024, hidden_size=512,
                 nb_channels=2, sample_rate=44100, nb_layers=3, max_bin=None, **_):
        super().__init__()
        self.sources = sources
        self.in_chan = in_chan
        self.n_hop = n_hop
        self.sample_rate = sample_rate
        self.nb_output_bins = in_chan // 2 + 1
        self.max_bin = int(max_bin) if max_bin else self.nb_output_bins
        self.encoder = nn.Sequential(_STFT(window_length, in_chan, n_hop), _Spectrogram())
        self.layer_enc = nn.ModuleDict(
            {s: _Enc(self.max_bin, hidden_size, nb_channels) for s in sources})
        self.layer_lstm = nn.ModuleDict({
            s: nn.LSTM(input_size=hidden_size, hidden_size=hidden_size // 2,
                       num_layers=nb_layers, bidirectional=True, batch_first=False,
                       dropout=0.4) for s in sources})
        self.layer_dec = nn.ModuleDict(
            {s: _Dec(self.nb_output_bins, hidden_size, nb_channels) for s in sources})
        ms = {}
        for s in sources:
            ms[f"input_mean_{s}"] = nn.Parameter(torch.zeros(self.max_bin))
            ms[f"input_scale_{s}"] = nn.Parameter(torch.ones(self.max_bin))
            ms[f"output_mean_{s}"] = nn.Parameter(torch.ones(self.nb_output_bins))
            ms[f"output_scale_{s}"] = nn.Parameter(torch.ones(self.nb_output_bins))
        self.mean_scale = nn.ParameterDict(ms)
        self.decoder = nn.Module()
        self.decoder.window = self.encoder[0].window

    def forward(self, wav):
        mixture, _ = self.encoder(wav)
        masks = self.forward_masker(mixture.clone())
        return torch.stack([mixture * masks[i] for i in range(len(self.sources))])

    def forward_masker(self, input_spec):
        shapes = input_spec.data.shape
        x = input_spec[..., : self.max_bin]
        inputs = [x] + [x.clone() for _ in self.sources[1:]]
        for i, src in enumerate(self.sources):
            inputs[i] += self.mean_scale[f"input_mean_{src}"]
            inputs[i] *= self.mean_scale[f"input_scale_{src}"]
            inputs[i] = self.layer_enc[src](inputs[i], shapes)
        cross_1 = sum(inputs) / len(self.sources)
        cross_2 = 0.0
        for i, src in enumerate(self.sources):
            tmp_lstm_out = self.layer_lstm[src](cross_1)
            cross_2 += torch.cat([inputs[i], tmp_lstm_out[0]], -1)
        cross_2 /= len(self.sources)
        masks = []
        for src in self.sources:
            x_tmp = self.layer_dec[src](cross_2, shapes)
            x_tmp *= self.mean_scale[f"output_scale_{src}"]
            x_tmp += self.mean_scale[f"output_mean_{src}"]
            masks.append(F.relu(x_tmp))
        return torch.stack(masks, dim=0)


def separate(audio, model, niter):
    """eval.py's `separate` (softmask off, no residual): stems by source."""
    audio_torch = torch.tensor(audio.T[None, ...]).float()
    with torch.no_grad():
        masked = model(audio_torch)
        mag, phase = model.encoder(audio_torch)
    V = np.transpose(masked[:, :, 0].numpy(), (1, 3, 2, 0))
    X = (mag.permute(1, 2, 3, 0) * torch.exp(1j * phase)).numpy()
    X = X[0].transpose(2, 1, 0)
    Y = norbert.wiener(V, X.astype(np.complex128), niter, use_softmask=False)
    stems = []
    for j in range(len(model.sources)):
        _, a = scipy.signal.istft(Y[..., j].T / (model.in_chan / 2), model.sample_rate,
                                  nperseg=model.in_chan, noverlap=model.in_chan - model.n_hop,
                                  boundary=True)
        stems.append(a)
    # V: frames, bins, channels, sources
    return V, np.array(stems)


def signal(n, rate):
    """The Rust tests' signal (`tests/common/mod.rs`): bass, kick, hats,
    a voice through two formants and a pad, a little apart in the stereo."""
    state = 12345
    noise = np.empty(n)
    for i in range(n):
        state = (1664525 * state + 1013904223) & 0xFFFFFFFF
        noise[i] = state / 4294967296.0 * 2.0 - 1.0
    t = np.arange(n, dtype=np.float64) / rate
    tau = 2.0 * math.pi
    bass = 0.22 * np.sin(tau * 55.0 * t) + 0.08 * np.sin(tau * 110.0 * t)
    tk = np.remainder(t, 0.5)
    kick = 0.5 * np.sin(tau * (50.0 * tk + 2.4 * (1.0 - np.exp(-tk / 0.03)))) * np.exp(-tk / 0.12)
    th = np.remainder(t - 0.25, 0.5)
    hat = 0.12 * noise * np.exp(-th / 0.025)
    f0 = np.where(t < 1.5, 220.0, 261.63)
    tn = np.remainder(t, 1.5)
    env = np.minimum(1.0, tn / 0.05) * np.exp(-tn / 1.2)
    voice = np.zeros(n)
    for h in range(1, 16):
        f = h * f0
        g = np.exp(-(((f - 700.0) / 150.0) ** 2)) + 0.6 * np.exp(-(((f - 1200.0) / 200.0) ** 2)) + 0.05
        voice += g * np.sin(tau * f * t) / h
    voice *= 0.3 * env
    pad = 0.07 * (np.sin(tau * 329.63 * t) + np.sin(tau * 493.88 * t))
    left = bass + kick + voice + 0.8 * hat + 0.3 * pad
    right = bass + kick + voice + 0.4 * hat + 0.9 * pad
    return np.stack([left, right], axis=1).astype(np.float32)


def tiny_model():
    torch.manual_seed(20261029)
    model = XUMX(SOURCES, window_length=256, in_chan=256, n_hop=64, hidden_size=8,
                 nb_channels=2, sample_rate=44100, nb_layers=3, max_bin=64)
    with torch.no_grad():
        for name, p in model.named_parameters():
            if name.endswith("window"):
                continue
            if "mean_scale.input_mean" in name:
                p.uniform_(-2.0, 0.0)
            elif "mean_scale.input_scale" in name:
                p.uniform_(0.2, 1.0)
            elif "mean_scale.output" in name or name.endswith(("1.weight", "4.weight")):
                p.uniform_(0.5, 1.5)
            elif name.endswith(("1.bias", "4.bias")):
                p.uniform_(-0.3, 0.3)
            else:
                p.uniform_(-0.6, 0.6)
        for name, b in model.named_buffers():
            if name.endswith("running_mean"):
                b.uniform_(-0.5, 0.5)
            elif name.endswith("running_var"):
                b.uniform_(0.5, 2.0)
    return model


def save_tiny(model, path):
    """As asteroid's `BaseModel.serialize` writes it (`max_bin` a numpy
    integer, as in the published checkpoint)."""
    package = {
        "model_name": "XUMX",
        "state_dict": model.state_dict(),
        "model_args": {
            "window_length": 256, "in_chan": 256, "n_hop": 64, "sample_rate": 44100,
            "sources": SOURCES, "hidden_size": 8, "nb_channels": 2, "input_mean": None,
            "input_scale": None, "max_bin": np.int64(64), "nb_layers": 3,
            "bidirectional": True, "spec_power": 1, "return_time_signals": False,
        },
        "infos": {"software_versions": {"torch_version": torch.__version__},
                  "note": "random weights for faderframe-separate's tests"},
    }
    path.parent.mkdir(parents=True, exist_ok=True)
    torch.save(package, str(path))


def load(path):
    # Weights only: the checkpoint's pickle may name nothing but tensors,
    # containers, the numpy integer asteroid stores `max_bin` as and the
    # torch version (a string).
    from numpy._core.multiarray import scalar
    allowed = [(scalar, "numpy.core.multiarray.scalar"), scalar, np.dtype, type(np.dtype("i8")),
               torch.torch_version.TorchVersion]
    with torch.serialization.safe_globals(allowed):
        conf = torch.load(str(path), map_location="cpu", weights_only=True)
    model = XUMX(**conf["model_args"])
    state = {k: v for k, v in conf["state_dict"].items()}
    missing, unexpected = model.load_state_dict(state, strict=False)
    unexpected = [k for k in unexpected if not k.endswith("num_batches_tracked")]
    if missing or unexpected:
        raise SystemExit(f"checkpoint mismatch: missing {missing}, unexpected {unexpected}")
    model.eval()
    return model


def g7(v):
    """Seven significant digits: as much as float32 holds."""
    return float(f"{v:.7g}")


def reference(model, n, picks, iterations):
    audio = signal(n, model.sample_rate)
    rng = np.random.default_rng(7)
    out = {"frames": n, "rate": model.sample_rate, "sources": model.sources,
           "input_sample": [g7(audio[i, c]) for i, c in [(0, 0), (n // 3, 1), (n - 1, 0)]]}
    V, _ = separate(audio, model, 0)
    nf, nb, nc, ns = V.shape
    idx = [(int(rng.integers(ns)), int(rng.integers(nf)), int(rng.integers(nc)),
            int(rng.integers(nb))) for _ in range(picks)]
    out["spectrogram"] = {"shape": [nf, nb, nc, ns],
                          "samples": [[s, f, c, b, g7(V[f, b, c, s])] for s, f, c, b in idx],
                          "frame_sums": [[g7(v) for v in row]
                                         for row in V.astype(np.float64).sum(axis=(1, 2)).T]}
    out["stems"] = {}
    for niter in iterations:
        _, stems = separate(audio, model, niter)
        ns, nc, length = stems.shape
        idx = [(int(rng.integers(ns)), int(rng.integers(nc)), int(rng.integers(length)))
               for _ in range(picks)]
        block = model.n_hop
        blocks = length // block
        rms = np.sqrt((stems[:, :, : blocks * block] ** 2)
                      .reshape(ns, nc, blocks, block).mean(axis=-1))
        out["stems"][str(niter)] = {
            "length": length,
            "samples": [[s, c, i, g7(stems[s, c, i])] for s, c, i in idx],
            "block": block,
            "block_rms": [[[g7(v) for v in ch] for ch in src] for src in rms],
        }
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--checkpoint", type=Path, help="the published pretrained_xumxl.pth")
    args = ap.parse_args()
    torch.set_num_threads(4)
    if args.checkpoint:
        model = load(args.checkpoint)
        # 129 hops: about 3 s.
        ref = reference(model, 129 * model.n_hop, 2000, [1])
        path = CRATE / "tests" / "reference-xumxl.json"
    else:
        tiny = CRATE / "tests" / "data" / "tiny-xumx.pth"
        save_tiny(tiny_model(), tiny)
        model = load(tiny)
        ref = reference(model, 160 * model.n_hop, 600, [0, 1, 2])
        path = CRATE / "tests" / "reference-tiny.json"
    path.write_text(json.dumps(ref, separators=(",", ":")) + "\n")
    print(f"wrote {path}")


if __name__ == "__main__":
    main()
