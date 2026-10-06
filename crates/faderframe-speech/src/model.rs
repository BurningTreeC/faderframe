//! Whisper's network (as in Hugging Face's `WhisperModel`): an encoder of
//! two convolutions and pre-norm transformer blocks over 1500 positions,
//! and a decoder of blocks with causal self-attention (cached) and
//! attention over the encoder's output; the logits are the token
//! embedding's transpose. Matrix products run on several threads.

use crate::SpeechError;
use crate::files::Weights;

/// The checkpoint's dimensions (`config.json`).
#[derive(Clone, Copy, Debug)]
pub struct Dims {
    pub d: usize,
    pub heads: usize,
    pub ffn: usize,
    pub encoder_layers: usize,
    pub decoder_layers: usize,
    pub vocab: usize,
    pub positions: usize,
    pub context: usize,
    pub mels: usize,
}

impl Dims {
    pub fn from_config(c: &serde_json::Value) -> Option<Self> {
        let u = |k: &str| c[k].as_u64().map(|v| v as usize);
        Some(Self {
            d: u("d_model")?,
            heads: u("encoder_attention_heads")?,
            ffn: u("encoder_ffn_dim")?,
            encoder_layers: u("encoder_layers")?,
            decoder_layers: u("decoder_layers")?,
            vocab: u("vocab_size")?,
            positions: u("max_source_positions")?,
            context: u("max_target_positions")?,
            mels: u("num_mel_bins")?,
        })
    }
}

/// A linear layer: `out × in` weights, optional bias.
struct Linear {
    w: Vec<f32>,
    b: Option<Vec<f32>>,
    out: usize,
    inp: usize,
}

struct Norm {
    g: Vec<f32>,
    b: Vec<f32>,
}

struct Attention {
    q: Linear,
    k: Linear,
    v: Linear,
    o: Linear,
}

struct Block {
    attn_norm: Norm,
    attn: Attention,
    /// Decoder only: attention over the encoder's output.
    cross: Option<(Norm, Attention)>,
    mlp_norm: Norm,
    fc1: Linear,
    fc2: Linear,
}

pub struct Model {
    pub dims: Dims,
    conv1: Linear,
    conv2: Linear,
    enc_pos: Vec<f32>,
    encoder: Vec<Block>,
    enc_norm: Norm,
    tokens: Vec<f32>,
    dec_pos: Vec<f32>,
    decoder: Vec<Block>,
    dec_norm: Norm,
}

/// How many threads products run on.
fn threads() -> usize {
    std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(16)
}

#[inline]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0.0f32; 8];
    let chunks = a.len() / 8;
    for c in 0..chunks {
        let (x, y) = (&a[c * 8..c * 8 + 8], &b[c * 8..c * 8 + 8]);
        for i in 0..8 {
            acc[i] += x[i] * y[i];
        }
    }
    let mut s: f32 = acc.iter().sum();
    for i in chunks * 8..a.len() {
        s += a[i] * b[i];
    }
    s
}

impl Linear {
    /// `x` (`n` rows of `inp`) times the weights: `n` rows of `out`.
    fn apply(&self, x: &[f32], n: usize) -> Vec<f32> {
        let mut y = vec![0.0f32; n * self.out];
        let t = threads();
        let row = |i: usize, dst: &mut [f32]| {
            let xi = &x[i * self.inp..(i + 1) * self.inp];
            for (j, d) in dst.iter_mut().enumerate() {
                *d = dot(xi, &self.w[j * self.inp..(j + 1) * self.inp])
                    + self.b.as_ref().map_or(0.0, |b| b[j]);
            }
        };
        if n >= t * 2 {
            let rows = n.div_ceil(t);
            std::thread::scope(|s| {
                for (c, chunk) in y.chunks_mut(rows * self.out).enumerate() {
                    s.spawn(move || {
                        for (k, dst) in chunk.chunks_mut(self.out).enumerate() {
                            row(c * rows + k, dst);
                        }
                    });
                }
            });
        } else {
            // Few rows (decoding): split the outputs instead.
            for i in 0..n {
                let xi = &x[i * self.inp..(i + 1) * self.inp];
                let dst = &mut y[i * self.out..(i + 1) * self.out];
                let cols = self.out.div_ceil(t);
                std::thread::scope(|s| {
                    for (c, part) in dst.chunks_mut(cols).enumerate() {
                        s.spawn(move || {
                            for (k, d) in part.iter_mut().enumerate() {
                                let j = c * cols + k;
                                *d = dot(xi, &self.w[j * self.inp..(j + 1) * self.inp])
                                    + self.b.as_ref().map_or(0.0, |b| b[j]);
                            }
                        });
                    }
                });
            }
        }
        y
    }
}

impl Norm {
    fn apply(&self, x: &[f32], d: usize) -> Vec<f32> {
        let mut y = vec![0.0f32; x.len()];
        for (src, dst) in x.chunks(d).zip(y.chunks_mut(d)) {
            let mean = src.iter().sum::<f32>() / d as f32;
            let var = src.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / d as f32;
            let inv = 1.0 / (var + 1e-5).sqrt();
            for i in 0..d {
                dst[i] = (src[i] - mean) * inv * self.g[i] + self.b[i];
            }
        }
        y
    }
}

/// The error function (Abramowitz–Stegun 7.1.26, |error| < 1.5e-7).
fn erf(x: f32) -> f32 {
    let t = 1.0 / (1.0 + 0.327_591_1 * x.abs());
    let y = 1.0
        - (((((1.061_405_4 * t - 1.453_152_1) * t) + 1.421_413_7) * t - 0.284_496_74) * t
            + 0.254_829_6)
            * t
            * (-x * x).exp();
    if x >= 0.0 { y } else { -y }
}

fn gelu(x: &mut [f32]) {
    for v in x {
        *v = 0.5 * *v * (1.0 + erf(*v * std::f32::consts::FRAC_1_SQRT_2));
    }
}

#[allow(clippy::too_many_arguments)] // the shapes, spelled out
/// Attention of `nq` queries over `nk` keys (rows of `d`, `heads` heads):
/// the values weighted by the softmax of the scaled scores; `causal` keeps
/// query `i` (at position `offset + i`) to keys up to it.
fn attend(
    q: &[f32],
    k: &[f32],
    v: &[f32],
    nq: usize,
    nk: usize,
    d: usize,
    heads: usize,
    causal: Option<usize>,
) -> Vec<f32> {
    let hd = d / heads;
    let scale = (hd as f32).powf(-0.5);
    let mut out = vec![0.0f32; nq * d];
    let t = threads();
    let rows = nq.div_ceil(t).max(1);
    std::thread::scope(|s| {
        for (c, chunk) in out.chunks_mut(rows * d).enumerate() {
            s.spawn(move || {
                let mut scores = vec![0.0f32; nk];
                for (r, dst) in chunk.chunks_mut(d).enumerate() {
                    let i = c * rows + r;
                    let upto = causal.map_or(nk, |off| (off + i + 1).min(nk));
                    for h in 0..heads {
                        let qi = &q[i * d + h * hd..i * d + (h + 1) * hd];
                        let mut top = f32::NEG_INFINITY;
                        for j in 0..upto {
                            let s = dot(qi, &k[j * d + h * hd..j * d + (h + 1) * hd]) * scale;
                            scores[j] = s;
                            top = top.max(s);
                        }
                        let mut sum = 0.0;
                        for s in &mut scores[..upto] {
                            *s = (*s - top).exp();
                            sum += *s;
                        }
                        let o = &mut dst[h * hd..(h + 1) * hd];
                        o.fill(0.0);
                        for j in 0..upto {
                            let p = scores[j] / sum;
                            let vj = &v[j * d + h * hd..j * d + (h + 1) * hd];
                            for x in 0..hd {
                                o[x] += p * vj[x];
                            }
                        }
                    }
                }
            });
        }
    });
    out
}

fn add(x: &mut [f32], y: &[f32]) {
    for (a, b) in x.iter_mut().zip(y) {
        *a += b;
    }
}

/// The cached keys and values of a decoder's layers for one window.
pub struct Cache {
    /// Per layer: self-attention keys and values so far.
    selfs: Vec<(Vec<f32>, Vec<f32>)>,
    /// Per layer: the encoder output's keys and values.
    cross: Vec<(Vec<f32>, Vec<f32>)>,
    pub len: usize,
}

impl Model {
    pub fn load(w: &Weights, dims: Dims) -> Result<Self, SpeechError> {
        let Dims {
            d,
            ffn,
            mels,
            vocab,
            positions,
            context,
            ..
        } = dims;
        let lin = |name: &str, out: usize, inp: usize, bias: bool| -> Result<Linear, SpeechError> {
            Ok(Linear {
                w: w.get(&format!("{name}.weight"), &[out, inp])?,
                b: if bias {
                    Some(w.get(&format!("{name}.bias"), &[out])?)
                } else {
                    None
                },
                out,
                inp,
            })
        };
        let norm = |name: &str| -> Result<Norm, SpeechError> {
            Ok(Norm {
                g: w.get(&format!("{name}.weight"), &[d])?,
                b: w.get(&format!("{name}.bias"), &[d])?,
            })
        };
        let attention = |p: &str| -> Result<Attention, SpeechError> {
            Ok(Attention {
                q: lin(&format!("{p}.q_proj"), d, d, true)?,
                k: lin(&format!("{p}.k_proj"), d, d, false)?,
                v: lin(&format!("{p}.v_proj"), d, d, true)?,
                o: lin(&format!("{p}.out_proj"), d, d, true)?,
            })
        };
        let block = |p: String, cross: bool| -> Result<Block, SpeechError> {
            Ok(Block {
                attn_norm: norm(&format!("{p}.self_attn_layer_norm"))?,
                attn: attention(&format!("{p}.self_attn"))?,
                cross: if cross {
                    Some((
                        norm(&format!("{p}.encoder_attn_layer_norm"))?,
                        attention(&format!("{p}.encoder_attn"))?,
                    ))
                } else {
                    None
                },
                mlp_norm: norm(&format!("{p}.final_layer_norm"))?,
                fc1: lin(&format!("{p}.fc1"), ffn, d, true)?,
                fc2: lin(&format!("{p}.fc2"), d, ffn, true)?,
            })
        };
        // Convolutions as linear layers over (channel, tap) columns.
        let conv = |name: &str, out: usize, inp: usize| -> Result<Linear, SpeechError> {
            Ok(Linear {
                w: w.get(&format!("{name}.weight"), &[out, inp, 3])?,
                b: Some(w.get(&format!("{name}.bias"), &[out])?),
                out,
                inp: inp * 3,
            })
        };
        Ok(Self {
            dims,
            conv1: conv("model.encoder.conv1", d, mels)?,
            conv2: conv("model.encoder.conv2", d, d)?,
            enc_pos: w.get("model.encoder.embed_positions.weight", &[positions, d])?,
            encoder: (0..dims.encoder_layers)
                .map(|i| block(format!("model.encoder.layers.{i}"), false))
                .collect::<Result<_, _>>()?,
            enc_norm: norm("model.encoder.layer_norm")?,
            tokens: w.get("model.decoder.embed_tokens.weight", &[vocab, d])?,
            dec_pos: w.get("model.decoder.embed_positions.weight", &[context, d])?,
            decoder: (0..dims.decoder_layers)
                .map(|i| block(format!("model.decoder.layers.{i}"), true))
                .collect::<Result<_, _>>()?,
            dec_norm: norm("model.decoder.layer_norm")?,
        })
    }

    /// The encoder's output for a window of mel frames (frame-major, `2 ×
    /// positions` frames of `mels`): `positions` rows of `d`.
    pub fn encode(&self, mel: &[f32]) -> Vec<f32> {
        let Dims {
            d,
            mels,
            positions,
            heads,
            ..
        } = self.dims;
        let frames = positions * 2;
        // conv1 (k 3, pad 1): columns (channel, tap) for each frame.
        let mut cols = vec![0.0f32; frames * mels * 3];
        for t in 0..frames {
            for c in 0..mels {
                for k in 0..3 {
                    let src = t as isize + k as isize - 1;
                    if src >= 0 && (src as usize) < frames {
                        cols[t * mels * 3 + c * 3 + k] = mel[src as usize * mels + c];
                    }
                }
            }
        }
        let mut x = self.conv1.apply(&cols, frames);
        gelu(&mut x);
        // conv2 (k 3, stride 2, pad 1).
        let mut cols = vec![0.0f32; positions * d * 3];
        for t in 0..positions {
            for c in 0..d {
                for k in 0..3 {
                    let src = (2 * t) as isize + k as isize - 1;
                    if src >= 0 && (src as usize) < frames {
                        cols[t * d * 3 + c * 3 + k] = x[src as usize * d + c];
                    }
                }
            }
        }
        let mut h = self.conv2.apply(&cols, positions);
        gelu(&mut h);
        add(&mut h, &self.enc_pos);
        for b in &self.encoder {
            let n = b.attn_norm.apply(&h, d);
            let (q, k, v) = (
                b.attn.q.apply(&n, positions),
                b.attn.k.apply(&n, positions),
                b.attn.v.apply(&n, positions),
            );
            let a = attend(&q, &k, &v, positions, positions, d, heads, None);
            add(&mut h, &b.attn.o.apply(&a, positions));
            let n = b.mlp_norm.apply(&h, d);
            let mut f = b.fc1.apply(&n, positions);
            gelu(&mut f);
            add(&mut h, &b.fc2.apply(&f, positions));
        }
        self.enc_norm.apply(&h, d)
    }

    /// A decoder cache for an encoded window.
    pub fn cache(&self, encoded: &[f32]) -> Cache {
        let n = self.dims.positions;
        Cache {
            selfs: vec![(Vec::new(), Vec::new()); self.decoder.len()],
            cross: self
                .decoder
                .iter()
                .map(|b| {
                    let a = &b
                        .cross
                        .as_ref()
                        .expect("decoder blocks attend to the encoder")
                        .1;
                    (a.k.apply(encoded, n), a.v.apply(encoded, n))
                })
                .collect(),
            len: 0,
        }
    }

    /// Feed `tokens` (after those cached); the logits after the last.
    pub fn step(&self, tokens: &[u32], cache: &mut Cache) -> Vec<f32> {
        let Dims {
            d,
            heads,
            positions,
            vocab,
            ..
        } = self.dims;
        let n = tokens.len();
        let start = cache.len;
        let mut h = vec![0.0f32; n * d];
        for (i, t) in tokens.iter().enumerate() {
            let t = (*t as usize).min(vocab - 1);
            let p = (start + i).min(self.dims.context - 1);
            for x in 0..d {
                h[i * d + x] = self.tokens[t * d + x] + self.dec_pos[p * d + x];
            }
        }
        for (l, b) in self.decoder.iter().enumerate() {
            let nrm = b.attn_norm.apply(&h, d);
            let q = b.attn.q.apply(&nrm, n);
            let (ks, vs) = &mut cache.selfs[l];
            ks.extend(b.attn.k.apply(&nrm, n));
            vs.extend(b.attn.v.apply(&nrm, n));
            let a = attend(&q, ks, vs, n, start + n, d, heads, Some(start));
            add(&mut h, &b.attn.o.apply(&a, n));
            if let Some((norm, cross)) = &b.cross {
                let nrm = norm.apply(&h, d);
                let q = cross.q.apply(&nrm, n);
                let (ck, cv) = &cache.cross[l];
                let a = attend(&q, ck, cv, n, positions, d, heads, None);
                add(&mut h, &cross.o.apply(&a, n));
            }
            let nrm = b.mlp_norm.apply(&h, d);
            let mut f = b.fc1.apply(&nrm, n);
            gelu(&mut f);
            add(&mut h, &b.fc2.apply(&f, n));
        }
        cache.len += n;
        let last = &h[(n - 1) * d..n * d];
        let last = self.dec_norm.apply(last, d);
        // Logits: the token embedding's transpose.
        let mut logits = vec![0.0f32; vocab];
        let t = threads();
        let cols = vocab.div_ceil(t);
        std::thread::scope(|s| {
            for (c, part) in logits.chunks_mut(cols).enumerate() {
                let last = &last;
                s.spawn(move || {
                    for (k, dst) in part.iter_mut().enumerate() {
                        let j = c * cols + k;
                        *dst = dot(last, &self.tokens[j * d..(j + 1) * d]);
                    }
                });
            }
        });
        logits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gelu_and_erf_are_exact_enough() {
        for (x, want) in [
            (0.0f32, 0.0f32),
            (0.5, 0.520_499_9),
            (1.0, 0.842_700_8),
            (-2.0, -0.995_322_3),
        ] {
            assert!((erf(x) - want).abs() < 2e-6, "{x}");
        }
        let mut v = [1.0f32, -1.0, 3.0];
        gelu(&mut v);
        assert!((v[0] - 0.841_344_7).abs() < 1e-5 && (v[1] + 0.158_655_3).abs() < 1e-5);
    }

    #[test]
    fn attention_weights_by_the_softmax() {
        // One head, two keys: the query matches the second.
        let q = [0.0f32, 10.0];
        let k = [1.0f32, 0.0, 0.0, 1.0];
        let v = [1.0f32, 2.0, 3.0, 4.0];
        let out = attend(&q, &k, &v, 1, 2, 2, 1, None);
        let s = 10.0f32 * 2f32.powf(-0.5);
        let p = s.exp() / (1.0 + s.exp());
        assert!((out[0] - (1.0 * (1.0 - p) + 3.0 * p)).abs() < 1e-5);
        // Causal at position 0: only the first key.
        let out = attend(&q, &k, &v, 1, 2, 2, 1, Some(0));
        assert_eq!(out, [1.0, 2.0]);
    }
}
