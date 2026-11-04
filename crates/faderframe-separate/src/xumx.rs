//! CrossNet-Open-Unmix (X-UMX; Sawata, Uhlich, Takahashi, Mitsufuji,
//! ICASSP 2021) as asteroid defines it (`asteroid/models/x_umx.py`, MIT):
//! one Open-Unmix network per source, bridged.
//!
//! For each source the mixture's magnitudes up to `max_bin` are shifted
//! and scaled, encoded (a linear layer, a batch norm, tanh) to `hidden`
//! features; the sources' encodings are averaged (the first bridge) and
//! each source's three-layer bidirectional LSTM runs over that average;
//! each source's encoding and LSTM output side by side are averaged again
//! (the second bridge) and each source decodes the average (linear, batch
//! norm, ReLU, linear, batch norm) into a mask for every bin and channel,
//! scaled and shifted, through a ReLU. A source's estimate is the
//! mixture's magnitude times its mask.

use crate::SeparateError;
use crate::nn::{Affine, LstmCell, matmul, parallel};
use crate::torch::{Checkpoint, Value};

/// The model's settings (asteroid's `model_args`).
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub sources: Vec<String>,
    pub sample_rate: u32,
    pub n_fft: usize,
    pub hop: usize,
    pub hidden: usize,
    pub layers: usize,
    pub channels: usize,
    /// Bins the network reads (the rest are only masked).
    pub max_bin: usize,
}

impl Config {
    pub fn bins(&self) -> usize {
        self.n_fft / 2 + 1
    }
}

/// One source's network.
struct Branch {
    input_mean: Vec<f32>,
    input_scale: Vec<f32>,
    /// `hidden × channels·max_bin`.
    fc1: Vec<f32>,
    bn1: Affine,
    /// Per layer: forward, backward.
    lstm: Vec<[LstmCell; 2]>,
    /// `hidden × 2·hidden`.
    fc2: Vec<f32>,
    bn2: Affine,
    /// `channels·bins × hidden`.
    fc3: Vec<f32>,
    /// The last batch norm with the output scale and mean folded in.
    out: Affine,
}

pub struct XUmx {
    pub config: Config,
    branches: Vec<Branch>,
}

fn int_arg(args: &Value, key: &str) -> Option<usize> {
    args.get(key)?
        .as_int()
        .and_then(|v| usize::try_from(v).ok())
}

impl XUmx {
    /// Load an asteroid X-UMX checkpoint (`torch.save` of `model_name`,
    /// `state_dict`, `model_args`).
    pub fn load(ck: &Checkpoint) -> Result<Self, SeparateError> {
        let fmt = |m: String| SeparateError::Format(m);
        let root = &ck.root;
        if let Some(name) = root.get("model_name")
            && name.as_str() != Some("XUMX")
        {
            return Err(fmt(format!(
                "the checkpoint holds {:?}, not an X-UMX model",
                name.as_str().unwrap_or("something else")
            )));
        }
        let args = root
            .get("model_args")
            .ok_or_else(|| fmt("the checkpoint has no model_args".into()))?;
        let state = root
            .get("state_dict")
            .ok_or_else(|| fmt("the checkpoint has no state_dict".into()))?;
        let tensors: std::collections::HashMap<String, crate::torch::Tensor> = state
            .entries()
            .into_iter()
            .filter_map(|(k, v)| match v {
                Value::Tensor(t) => Some((k, t)),
                _ => None,
            })
            .collect();
        let need = |k: &str| int_arg(&args, k).ok_or_else(|| fmt(format!("model_args: {k}")));
        let sources: Vec<String> = args
            .get("sources")
            .and_then(|s| s.items())
            .map(|v| {
                v.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .ok_or_else(|| fmt("model_args: sources".into()))?;
        let n_fft = need("in_chan")?;
        let window = int_arg(&args, "window_length").unwrap_or(n_fft);
        let channels = int_arg(&args, "nb_channels").unwrap_or(2);
        let hidden = need("hidden_size")?;
        if window != n_fft {
            return Err(fmt(format!(
                "a window of {window} for {n_fft}-point transforms"
            )));
        }
        if channels != 2 {
            return Err(fmt(format!(
                "a model for {channels} channels (stereo only)"
            )));
        }
        if args.get("bidirectional").and_then(|b| b.as_bool()) == Some(false) {
            return Err(fmt("a one-way LSTM model".into()));
        }
        if args.get("spec_power").and_then(|b| b.as_int()).unwrap_or(1) != 1 {
            return Err(fmt("a power-spectrogram model".into()));
        }
        if sources.is_empty() || hidden % 2 != 0 {
            return Err(fmt("model_args: sources or hidden_size".into()));
        }
        let bins = n_fft / 2 + 1;
        let read = |name: &str, shape: &[usize]| -> Result<Vec<f32>, SeparateError> {
            let t = tensors
                .get(name)
                .ok_or_else(|| fmt(format!("missing weight {name}")))?;
            if t.shape != shape {
                return Err(fmt(format!("{name}: shape {:?}, not {shape:?}", t.shape)));
            }
            ck.read_f32(t)
        };
        // `max_bin` is a numpy integer in asteroid's files; the encoder's
        // width says it too.
        let max_bin = match int_arg(&args, "max_bin") {
            Some(m) if m > 0 => m,
            _ => {
                let first = format!("layer_enc.{}.enc.0.weight", sources[0]);
                tensors
                    .get(&first)
                    .and_then(|t| t.shape.get(1).copied())
                    .map(|w| w / channels)
                    .ok_or_else(|| fmt("model_args: max_bin".into()))?
            }
        };
        let layers = need("nb_layers")?;
        let h = hidden / 2;
        let mut branches = Vec::with_capacity(sources.len());
        for s in &sources {
            let bn = |prefix: &str, n: usize| -> Result<Affine, SeparateError> {
                Ok(Affine::batch_norm(
                    &read(&format!("{prefix}.weight"), &[n])?,
                    &read(&format!("{prefix}.bias"), &[n])?,
                    &read(&format!("{prefix}.running_mean"), &[n])?,
                    &read(&format!("{prefix}.running_var"), &[n])?,
                ))
            };
            let mut lstm = Vec::with_capacity(layers);
            for l in 0..layers {
                let input = hidden;
                let cell = |suffix: &str| -> Result<LstmCell, SeparateError> {
                    let p = format!("layer_lstm.{s}");
                    let b_ih = read(&format!("{p}.bias_ih_l{l}{suffix}"), &[4 * h])?;
                    let b_hh = read(&format!("{p}.bias_hh_l{l}{suffix}"), &[4 * h])?;
                    Ok(LstmCell {
                        w_ih: read(&format!("{p}.weight_ih_l{l}{suffix}"), &[4 * h, input])?,
                        w_hh: read(&format!("{p}.weight_hh_l{l}{suffix}"), &[4 * h, h])?,
                        bias: b_ih.iter().zip(&b_hh).map(|(a, b)| a + b).collect(),
                        hidden: h,
                        input,
                    })
                };
                lstm.push([cell("")?, cell("_reverse")?]);
            }
            let bn3 = bn(&format!("layer_dec.{s}.dec.4"), channels * bins)?;
            let o_scale = read(&format!("mean_scale.output_scale_{s}"), &[bins])?;
            let o_mean = read(&format!("mean_scale.output_mean_{s}"), &[bins])?;
            // (x·a + b)·scale + mean, per channel and bin.
            let mut out = bn3;
            for c in 0..channels {
                for f in 0..bins {
                    let i = c * bins + f;
                    out.scale[i] *= o_scale[f];
                    out.shift[i] = out.shift[i] * o_scale[f] + o_mean[f];
                }
            }
            branches.push(Branch {
                input_mean: read(&format!("mean_scale.input_mean_{s}"), &[max_bin])?,
                input_scale: read(&format!("mean_scale.input_scale_{s}"), &[max_bin])?,
                fc1: read(
                    &format!("layer_enc.{s}.enc.0.weight"),
                    &[hidden, channels * max_bin],
                )?,
                bn1: bn(&format!("layer_enc.{s}.enc.1"), hidden)?,
                lstm,
                fc2: read(
                    &format!("layer_dec.{s}.dec.0.weight"),
                    &[hidden, 2 * hidden],
                )?,
                bn2: bn(&format!("layer_dec.{s}.dec.1"), hidden)?,
                fc3: read(
                    &format!("layer_dec.{s}.dec.3.weight"),
                    &[channels * bins, hidden],
                )?,
                out,
            });
        }
        Ok(Self {
            config: Config {
                sources,
                sample_rate: need("sample_rate")? as u32,
                n_fft,
                hop: need("n_hop")?,
                hidden,
                layers,
                channels,
                max_bin,
            },
            branches,
        })
    }

    /// The sources' magnitude estimates for `mag` (the mixture's
    /// magnitudes, per channel `frames × bins`): per source `frames ×
    /// channels × bins`. `step` hears each stage done (of
    /// [`Self::STAGES`]) and answers whether to go on.
    pub fn estimate(
        &self,
        mag: &[Vec<f32>],
        frames: usize,
        threads: usize,
        step: &mut dyn FnMut() -> bool,
    ) -> Option<Vec<Vec<f32>>> {
        let cfg = &self.config;
        let (hid, ch, bins, mb) = (cfg.hidden, cfg.channels, cfg.bins(), cfg.max_bin);
        let ns = self.branches.len();
        // Encoders.
        let mut enc: Vec<Vec<f32>> = (0..ns).map(|_| vec![0.0f32; frames * hid]).collect();
        for (b, e) in self.branches.iter().zip(enc.iter_mut()) {
            let width = ch * mb;
            let mut x = vec![0.0f32; frames * width];
            for t in 0..frames {
                for c in 0..ch {
                    let src = &mag[c][t * bins..t * bins + mb];
                    let dst = &mut x[t * width + c * mb..t * width + (c + 1) * mb];
                    for (((d, m), mean), scale) in dst
                        .iter_mut()
                        .zip(src)
                        .zip(&b.input_mean)
                        .zip(&b.input_scale)
                    {
                        *d = (m + mean) * scale;
                    }
                }
            }
            matmul(&x, width, &b.fc1, hid, e, threads);
            b.bn1.apply(e);
            e.iter_mut().for_each(|v| *v = v.tanh());
        }
        if !step() {
            return None;
        }
        // The first bridge.
        let cross1 = average(&enc);
        // The LSTMs, layer by layer, every source and direction at once.
        let mut layer_in: Vec<Vec<f32>> = vec![cross1; ns];
        for l in 0..cfg.layers {
            let cells: Vec<(usize, usize)> = (0..ns).flat_map(|s| [(s, 0), (s, 1)]).collect();
            // Projections first, spread over the threads...
            let mut projected: Vec<Vec<f32>> = Vec::with_capacity(cells.len());
            for &(s, d) in &cells {
                let cell = &self.branches[s].lstm[l][d];
                projected.push(cell.project(&layer_in[s], frames, threads));
            }
            // ...then the recurrences, one thread each.
            let mut outs: Vec<Vec<f32>> = (0..ns).map(|_| vec![0.0f32; frames * hid]).collect();
            {
                let mut halves: Vec<(usize, usize, &[f32], Vec<f32>)> = cells
                    .iter()
                    .zip(&projected)
                    .map(|(&(s, d), p)| (s, d, p.as_slice(), vec![0.0f32; frames * hid / 2]))
                    .collect();
                let jobs: Vec<&mut (usize, usize, &[f32], Vec<f32>)> = halves.iter_mut().collect();
                parallel(threads, jobs, |job| {
                    let (s, d, p, out) = (job.0, job.1, job.2, &mut job.3);
                    let cell = &self.branches[s].lstm[l][d];
                    cell.run(p, frames, d == 1, out, hid / 2, 0);
                });
                for (s, d, _, h) in halves {
                    let half = hid / 2;
                    for t in 0..frames {
                        outs[s][t * hid + d * half..t * hid + (d + 1) * half]
                            .copy_from_slice(&h[t * half..(t + 1) * half]);
                    }
                }
            }
            layer_in = outs;
            if !step() {
                return None;
            }
        }
        // The second bridge: each source's encoding and LSTM output side by
        // side, averaged.
        let cat: Vec<Vec<f32>> = enc
            .iter()
            .zip(&layer_in)
            .map(|(e, o)| {
                let mut v = vec![0.0f32; frames * 2 * hid];
                for t in 0..frames {
                    v[t * 2 * hid..t * 2 * hid + hid].copy_from_slice(&e[t * hid..(t + 1) * hid]);
                    v[t * 2 * hid + hid..(t + 1) * 2 * hid]
                        .copy_from_slice(&o[t * hid..(t + 1) * hid]);
                }
                v
            })
            .collect();
        drop(enc);
        drop(layer_in);
        let cross2 = average(&cat);
        drop(cat);
        if !step() {
            return None;
        }
        // Decoders: masks times the mixture.
        let mut estimates = Vec::with_capacity(ns);
        for b in &self.branches {
            let mut d1 = vec![0.0f32; frames * hid];
            matmul(&cross2, 2 * hid, &b.fc2, hid, &mut d1, threads);
            b.bn2.apply(&mut d1);
            d1.iter_mut().for_each(|v| *v = v.max(0.0));
            let mut m = vec![0.0f32; frames * ch * bins];
            matmul(&d1, hid, &b.fc3, ch * bins, &mut m, threads);
            b.out.apply(&mut m);
            for t in 0..frames {
                for c in 0..ch {
                    let row = &mut m[(t * ch + c) * bins..(t * ch + c + 1) * bins];
                    for (v, x) in row.iter_mut().zip(&mag[c][t * bins..(t + 1) * bins]) {
                        *v = v.max(0.0) * x;
                    }
                }
            }
            estimates.push(m);
            if !step() {
                return None;
            }
        }
        Some(estimates)
    }

    /// Stages [`Self::estimate`] reports.
    pub fn stages(&self) -> usize {
        2 + self.config.layers + self.branches.len()
    }
}

/// The element-wise mean of equally long vectors, summed in order (as
/// Python's `sum(…) / n`).
fn average(v: &[Vec<f32>]) -> Vec<f32> {
    let n = v.len() as f32;
    let mut out = vec![0.0f32; v[0].len()];
    for x in v {
        for (o, a) in out.iter_mut().zip(x) {
            *o += a;
        }
    }
    out.iter_mut().for_each(|o| *o /= n);
    out
}
