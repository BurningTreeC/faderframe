//! The model's files: weights (safetensors), vocabulary and settings, as
//! Hugging Face publishes OpenAI's Whisper checkpoints.

use crate::SpeechError;
use std::collections::HashMap;
use std::path::Path;

/// The files a model directory holds.
pub const FILES: [&str; 5] = [
    "config.json",
    "generation_config.json",
    "preprocessor_config.json",
    "vocab.json",
    "model.safetensors",
];

/// Where a checkpoint's file is published.
pub fn url(checkpoint: &str, file: &str) -> String {
    format!("https://huggingface.co/openai/{checkpoint}/resolve/main/{file}")
}

/// Named f32 tensors (shape, data).
pub struct Weights {
    tensors: HashMap<String, (Vec<usize>, Vec<f32>)>,
}

impl Weights {
    /// Read a safetensors file (F32 or F16 tensors).
    pub fn load(path: &Path) -> Result<Self, SpeechError> {
        let bytes =
            std::fs::read(path).map_err(|e| SpeechError::Io(path.display().to_string(), e))?;
        let bad = |m: &str| SpeechError::Format(format!("{}: {m}", path.display()));
        let n = bytes
            .get(..8)
            .map(|b| u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as usize)
            .ok_or_else(|| bad("header"))?;
        let head: serde_json::Value =
            serde_json::from_slice(bytes.get(8..8 + n).ok_or_else(|| bad("header"))?)
                .map_err(|e| bad(&e.to_string()))?;
        let data = &bytes[8 + n..];
        let mut tensors = HashMap::new();
        for (name, info) in head.as_object().ok_or_else(|| bad("header"))? {
            if name == "__metadata__" {
                continue;
            }
            let dtype = info["dtype"].as_str().unwrap_or("");
            let shape: Vec<usize> = info["shape"]
                .as_array()
                .ok_or_else(|| bad("shape"))?
                .iter()
                .map(|d| d.as_u64().unwrap_or(0) as usize)
                .collect();
            let off = info["data_offsets"]
                .as_array()
                .ok_or_else(|| bad("offsets"))?;
            let (a, b) = (
                off.first().and_then(|v| v.as_u64()).unwrap_or(0) as usize,
                off.get(1).and_then(|v| v.as_u64()).unwrap_or(0) as usize,
            );
            let raw = data.get(a..b).ok_or_else(|| bad("data"))?;
            let values: Vec<f32> = match dtype {
                "F32" => raw
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| f32::from_le_bytes(*c))
                    .collect(),
                "F16" => raw
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| f16_to_f32(u16::from_le_bytes(*c)))
                    .collect(),
                other => return Err(bad(&format!("{name}: dtype {other}"))),
            };
            tensors.insert(name.clone(), (shape, values));
        }
        Ok(Self { tensors })
    }

    /// A tensor's data (checked against the shape expected).
    pub fn get(&self, name: &str, shape: &[usize]) -> Result<Vec<f32>, SpeechError> {
        let (s, v) = self
            .tensors
            .get(name)
            .ok_or_else(|| SpeechError::Format(format!("missing weight {name}")))?;
        if s != shape {
            return Err(SpeechError::Format(format!(
                "{name}: shape {s:?}, not {shape:?}"
            )));
        }
        Ok(v.clone())
    }
}

fn f16_to_f32(h: u16) -> f32 {
    let sign = u32::from(h >> 15) << 31;
    let exp = u32::from((h >> 10) & 0x1f);
    let frac = u32::from(h & 0x3ff);
    let bits = match exp {
        0 if frac == 0 => sign,
        0 => {
            // Subnormal: normalise.
            let mut e = 127 - 15 + 1;
            let mut f = frac;
            while f & 0x400 == 0 {
                f <<= 1;
                e -= 1;
            }
            sign | (e << 23) | ((f & 0x3ff) << 13)
        }
        31 => sign | 0x7f80_0000 | (frac << 13),
        _ => sign | ((exp + 127 - 15) << 23) | (frac << 13),
    };
    f32::from_bits(bits)
}

/// Read a JSON file.
pub fn json(path: &Path) -> Result<serde_json::Value, SpeechError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| SpeechError::Io(path.display().to_string(), e))?;
    serde_json::from_str(&text).map_err(|e| SpeechError::Format(format!("{}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_floats_convert() {
        assert_eq!(f16_to_f32(0x3c00), 1.0);
        assert_eq!(f16_to_f32(0xc000), -2.0);
        assert_eq!(f16_to_f32(0x0000), 0.0);
        assert!((f16_to_f32(0x3555) - 0.333_25).abs() < 1e-4);
        assert!((f16_to_f32(0x0001) - 5.96e-8).abs() < 1e-9);
    }
}
