//! Speech and lyrics transcription: OpenAI's Whisper (code MIT, the
//! checkpoints Apache-2.0 as Hugging Face publishes them) in plain Rust.
//!
//! A checkpoint's files ([`files::FILES`], downloaded once by the session)
//! give the weights, the vocabulary, the mel filters and the special
//! tokens. [`Whisper::transcribe`] follows the reference transcription
//! loop: the log-mel spectrogram of the whole recording (16 kHz mono) with
//! 30 s of silence after it, decoded a 30 s window at a time — the
//! language detected once from the first window (or given), then greedy
//! decoding with timestamps under Whisper's timestamp rules (they come in
//! pairs, never go back, the first within a second) — each window's
//! segments placed by their timestamps, the next window starting where
//! the last complete segment ended. With [`Options::word_timestamps`]
//! each segment also carries its words and where each is heard
//! ([`timing`]: the alignment heads' attention, warped through the text).

#![forbid(unsafe_code)]

pub mod files;
pub mod mel;
pub mod model;
pub mod timing;
pub mod tokens;

use model::{Dims, Model};
use std::collections::HashMap;
use std::path::Path;
pub use timing::Word;

#[derive(Debug, thiserror::Error)]
pub enum SpeechError {
    #[error("{0}: {1}")]
    Io(String, std::io::Error),
    #[error("the speech model: {0}")]
    Format(String),
}

/// A transcribed stretch: seconds from the start and its text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
    /// Its words and their times (with [`Options::word_timestamps`]).
    pub words: Vec<Word>,
}

/// How to transcribe.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Options {
    /// Language code ("en", "de", …); `None` detects it.
    pub language: Option<String>,
    /// Time every word too (one more decoder pass a window).
    pub word_timestamps: bool,
}

/// Languages written without spaces between words (their words are
/// timed character by character).
const UNSPACED: [&str; 6] = ["zh", "ja", "th", "lo", "my", "yue"];

/// Special tokens.
struct Special {
    sot: u32,
    eot: u32,
    transcribe: u32,
    no_timestamps: u32,
    /// The first timestamp token (0.00 s; each next 0.02 s later).
    timestamp_begin: u32,
    languages: HashMap<String, u32>,
    suppress: Vec<u32>,
    begin_suppress: Vec<u32>,
    max_initial_timestamp: u32,
    /// The cross-attention heads (decoder layer, head) that follow the
    /// audio: the checkpoint's, else every head of the upper half.
    alignment_heads: Vec<(usize, usize)>,
}

pub struct Whisper {
    model: Model,
    filters: Vec<f32>,
    text: tokens::Detokenizer,
    special: Special,
}

/// Mel frames a window holds (30 s).
const WINDOW_FRAMES: usize = 3000;
/// Tokens sampled a window at most.
const SAMPLE_LEN: usize = 224;

fn ids(v: &serde_json::Value) -> Vec<u32> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_u64().map(|x| x as u32))
                .collect()
        })
        .unwrap_or_default()
}

impl Whisper {
    /// Load a checkpoint from a directory holding [`files::FILES`].
    pub fn load(dir: &Path) -> Result<Self, SpeechError> {
        let config = files::json(&dir.join("config.json"))?;
        let gen_cfg = files::json(&dir.join("generation_config.json"))?;
        let pre = files::json(&dir.join("preprocessor_config.json"))?;
        let vocab = files::json(&dir.join("vocab.json"))?;
        let dims =
            Dims::from_config(&config).ok_or_else(|| SpeechError::Format("config.json".into()))?;
        let weights = files::Weights::load(&dir.join("model.safetensors"))?;
        let model = Model::load(&weights, dims)?;
        drop(weights);
        let filters =
            mel::filters(&pre).ok_or_else(|| SpeechError::Format("mel filters".into()))?;
        let text = tokens::Detokenizer::new(&vocab)
            .ok_or_else(|| SpeechError::Format("vocab.json".into()))?;
        let u = |k: &str| gen_cfg[k].as_u64().map(|v| v as u32);
        let no_timestamps = u("no_timestamps_token_id")
            .ok_or_else(|| SpeechError::Format("no_timestamps_token_id".into()))?;
        let languages = gen_cfg["lang_to_id"]
            .as_object()
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| {
                        Some((
                            k.trim_start_matches("<|")
                                .trim_end_matches("|>")
                                .to_string(),
                            v.as_u64()? as u32,
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut alignment_heads: Vec<(usize, usize)> = gen_cfg["alignment_heads"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|p| {
                        Some((p.get(0)?.as_u64()? as usize, p.get(1)?.as_u64()? as usize))
                    })
                    .filter(|&(l, h)| l < dims.decoder_layers && h < dims.heads)
                    .collect()
            })
            .unwrap_or_default();
        if alignment_heads.is_empty() {
            alignment_heads = (dims.decoder_layers / 2..dims.decoder_layers)
                .flat_map(|l| (0..dims.heads).map(move |h| (l, h)))
                .collect();
        }
        let special = Special {
            sot: u("decoder_start_token_id").unwrap_or(50258),
            eot: u("eos_token_id").unwrap_or(50257),
            transcribe: gen_cfg["task_to_id"]["transcribe"]
                .as_u64()
                .map_or(50359, |v| v as u32),
            no_timestamps,
            timestamp_begin: no_timestamps + 1,
            languages,
            suppress: ids(&gen_cfg["suppress_tokens"]),
            begin_suppress: ids(&gen_cfg["begin_suppress_tokens"]),
            max_initial_timestamp: u("max_initial_timestamp_index").unwrap_or(50),
            alignment_heads,
        };
        Ok(Self {
            model,
            filters,
            text,
            special,
        })
    }

    /// The language codes the checkpoint knows.
    pub fn languages(&self) -> Vec<String> {
        let mut v: Vec<String> = self.special.languages.keys().cloned().collect();
        v.sort();
        v
    }

    /// The most likely language of an encoded window.
    fn detect_language(&self, encoded: &[f32]) -> Option<u32> {
        if self.special.languages.is_empty() {
            return None;
        }
        let mut cache = self.model.cache(encoded);
        let logits = self.model.step(&[self.special.sot], &mut cache);
        self.special
            .languages
            .values()
            .copied()
            .max_by(|a, b| logits[*a as usize].total_cmp(&logits[*b as usize]))
    }

    /// Greedy decoding of one window under the timestamp rules; the
    /// sampled tokens and how likely each was.
    fn decode(&self, encoded: &[f32], language: Option<u32>) -> (Vec<u32>, Vec<f32>) {
        let sp = &self.special;
        let mut prompt = vec![sp.sot];
        prompt.extend(language);
        prompt.push(sp.transcribe);
        let mut cache = self.model.cache(encoded);
        let mut logits = self.model.step(&prompt, &mut cache);
        let mut out: Vec<u32> = Vec::new();
        let mut likely: Vec<f32> = Vec::new();
        let tb = sp.timestamp_begin as usize;
        for _ in 0..SAMPLE_LEN {
            if cache.len >= self.model.dims.context {
                break;
            }
            let ninf = f32::NEG_INFINITY;
            for t in &sp.suppress {
                if let Some(l) = logits.get_mut(*t as usize) {
                    *l = ninf;
                }
            }
            if out.is_empty() {
                for t in &sp.begin_suppress {
                    if let Some(l) = logits.get_mut(*t as usize) {
                        *l = ninf;
                    }
                }
            }
            // The timestamp rules.
            logits[sp.no_timestamps as usize] = ninf;
            let last_ts = out.last().is_some_and(|t| *t >= sp.timestamp_begin);
            let penultimate_ts = out.len() < 2 || out[out.len() - 2] >= sp.timestamp_begin;
            if last_ts {
                if penultimate_ts {
                    // A pair closed: text (or the end) next.
                    for l in &mut logits[tb..] {
                        *l = ninf;
                    }
                } else {
                    // An opened stretch: a timestamp (or the end) next.
                    for l in &mut logits[..sp.eot as usize] {
                        *l = ninf;
                    }
                }
            }
            if let Some(last) = out.iter().rev().find(|t| **t >= sp.timestamp_begin) {
                // Timestamps never go back (nor repeat one closing a pair).
                let floor = if last_ts && !penultimate_ts {
                    *last as usize
                } else {
                    *last as usize + 1
                };
                let end = floor.min(logits.len());
                for l in &mut logits[tb..end] {
                    *l = ninf;
                }
            }
            if out.is_empty() {
                // It starts with a timestamp, within the first second.
                for l in &mut logits[..tb] {
                    *l = ninf;
                }
                let most = tb + sp.max_initial_timestamp as usize + 1;
                for l in logits.iter_mut().skip(most) {
                    *l = ninf;
                }
            }
            // When the timestamps together are likelier than any text
            // token, a timestamp.
            let lse = |s: &[f32]| {
                let m = s.iter().copied().fold(ninf, f32::max);
                if m == ninf {
                    return ninf;
                }
                m + s.iter().map(|v| (v - m).exp()).sum::<f32>().ln()
            };
            let all = lse(&logits);
            let ts_logprob = lse(&logits[tb..]) - all;
            let text_max = logits[..tb].iter().copied().fold(ninf, f32::max) - all;
            if ts_logprob > text_max {
                for l in &mut logits[..tb] {
                    *l = ninf;
                }
            }
            let next = logits
                .iter()
                .enumerate()
                .fold(
                    (0usize, ninf),
                    |b, (i, v)| if *v > b.1 { (i, *v) } else { b },
                )
                .0 as u32;
            if next == sp.eot {
                break;
            }
            out.push(next);
            likely.push((logits[next as usize] - all).exp());
            logits = self.model.step(&[next], &mut cache);
        }
        (out, likely)
    }

    /// The words of a window's text tokens and where each is heard
    /// (seconds from `offset`; `frames`: mel frames of audio in the
    /// window).
    fn time_words(
        &self,
        encoded: &[f32],
        language: Option<u32>,
        text: &[u32],
        probabilities: &[f32],
        frames: usize,
        offset: f64,
    ) -> Vec<Word> {
        let sp = &self.special;
        if text.is_empty() {
            return Vec::new();
        }
        let mut tokens = vec![sp.sot];
        tokens.extend(language);
        tokens.push(sp.transcribe);
        let prompt = tokens.len();
        tokens.push(sp.no_timestamps);
        tokens.extend_from_slice(text);
        tokens.push(sp.eot);
        // Encoder positions are two mel frames each.
        let positions = (frames / 2).max(1);
        let scores = self
            .model
            .cross_scores(&tokens, encoded, &sp.alignment_heads, positions);
        let starts = timing::token_starts(&scores, tokens.len(), positions, prompt);
        let pieces: Vec<Vec<u8>> = text.iter().map(|t| self.text.piece(*t).to_vec()).collect();
        let spaced = !language.is_some_and(|l| {
            sp.languages
                .iter()
                .any(|(code, id)| *id == l && UNSPACED.contains(&code.as_str()))
        });
        timing::words(&pieces, &starts, probabilities, spaced, offset)
    }

    /// Transcribe 16 kHz mono `audio` (see the module docs); `progress`
    /// hears how far it is (0…1).
    pub fn transcribe(
        &self,
        audio: &[f32],
        options: &Options,
        mut progress: impl FnMut(f32),
    ) -> Vec<Segment> {
        let sp = &self.special;
        let mut padded = audio.to_vec();
        padded.extend(std::iter::repeat_n(0.0, WINDOW_FRAMES * mel::HOP));
        let (mel, frames) = mel::log_mel(&padded, &self.filters);
        let content = audio.len() / mel::HOP;
        let mut language = options
            .language
            .as_ref()
            .and_then(|l| sp.languages.get(l).copied());
        let mut seek = 0usize;
        let mut segments = Vec::new();
        let secs = |frames: usize| frames as f64 * mel::HOP as f64 / mel::RATE as f64;
        while seek < content {
            progress(seek as f32 / content.max(1) as f32);
            let size = WINDOW_FRAMES.min(content - seek);
            let mut window = vec![0.0f32; WINDOW_FRAMES * mel::MELS];
            let take = WINDOW_FRAMES.min(frames.saturating_sub(seek));
            window[..take * mel::MELS]
                .copy_from_slice(&mel[seek * mel::MELS..(seek + take) * mel::MELS]);
            let encoded = self.model.encode(&window);
            if language.is_none() {
                language = self.detect_language(&encoded);
            }
            let (tokens, likely) = self.decode(&encoded, language);
            let offset = secs(seek);
            let ts = |t: u32| f64::from(t - sp.timestamp_begin) * 0.02;
            let is_ts: Vec<bool> = tokens.iter().map(|t| *t >= sp.timestamp_begin).collect();
            let single_ending =
                tokens.len() >= 2 && !is_ts[tokens.len() - 2] && is_ts[tokens.len() - 1];
            let consecutive: Vec<usize> = (1..tokens.len())
                .filter(|i| is_ts[*i] && is_ts[i - 1])
                .collect();
            // This window's segments: their text tokens (with how likely
            // each was) and times.
            let mut found: Vec<(Vec<u32>, Vec<f32>, f64, f64)> = Vec::new();
            let mut push = |range: std::ops::Range<usize>, start: f64, end: f64| {
                let (words, p): (Vec<u32>, Vec<f32>) = range
                    .filter(|&i| tokens[i] < sp.eot)
                    .map(|i| (tokens[i], likely[i]))
                    .unzip();
                if !self.text.decode(&words).trim().is_empty() {
                    found.push((words, p, offset + start, offset + end.max(start)));
                }
            };
            if !consecutive.is_empty() {
                let mut slices = consecutive.clone();
                if single_ending {
                    slices.push(tokens.len());
                }
                let mut last = 0;
                for s in slices {
                    let part = &tokens[last..s];
                    if let (Some(a), Some(b)) = (part.first(), part.last())
                        && *a >= sp.timestamp_begin
                        && *b >= sp.timestamp_begin
                    {
                        push(last..s, ts(*a), ts(*b));
                    }
                    last = s;
                }
                if single_ending {
                    seek += size;
                } else {
                    let last_ts = tokens[last - 1];
                    seek += ((last_ts - sp.timestamp_begin) as usize * 2).max(1);
                }
            } else {
                let duration = tokens
                    .iter()
                    .rev()
                    .find(|t| **t > sp.timestamp_begin)
                    .map_or(secs(size), |t| ts(*t));
                push(0..tokens.len(), 0.0, duration);
                seek += size;
            }
            let mut words = if options.word_timestamps {
                let text: Vec<u32> = found.iter().flat_map(|f| f.0.iter().copied()).collect();
                let p: Vec<f32> = found.iter().flat_map(|f| f.1.iter().copied()).collect();
                self.time_words(&encoded, language, &text, &p, size, offset)
                    .into_iter()
            } else {
                Vec::new().into_iter()
            }
            .peekable();
            for (text, _, start, end) in found {
                let line = self.text.decode(&text).trim().to_string();
                // The segment's words: as many tokens' worth as it has.
                let mut mine = Vec::new();
                let mut bytes = 0;
                let want: usize = text.iter().map(|t| self.text.piece(*t).len()).sum();
                while bytes < want
                    && let Some(w) = words.next()
                {
                    bytes += w.text.len();
                    mine.push(w);
                }
                segments.push(Segment {
                    start,
                    end,
                    text: line,
                    words: mine,
                });
            }
        }
        progress(1.0);
        segments
    }
}
