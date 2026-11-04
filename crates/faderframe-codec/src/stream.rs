//! The mix as a stream of Opus packets, and back (Listen Along in a live
//! session: the host's mix on a peer's speakers).
//!
//! Stereo at 48 kHz in frames of [`FRAME`] (20 ms) — the project rate is
//! resampled with the codec monitor's polyphase filters
//! ([`crate::resample`]) on the way in and out. [`StreamEncoder`] takes
//! the mix in pieces of any length and hands out a packet per frame;
//! [`StreamDecoder`] turns a packet (or a lost one: Opus conceals it)
//! back into stereo at the listener's rate.

use crate::resample::{RatePlan, Resampler};
use crate::{CodecError, FRAME};
#[cfg(not(faderframe_check_only))]
use crate::{clamp_kbps, resample::CODEC_RATE};

/// The bit rate the mix is streamed at unless told otherwise.
pub const STREAM_KBPS: u32 = 128;
/// The largest packet (bytes).
#[cfg(not(faderframe_check_only))]
const MAX_PACKET: usize = 4000;

#[cfg(not(faderframe_check_only))]
fn err(e: opus::Error) -> CodecError {
    CodecError::Opus(e.to_string())
}

/// Stereo at the project rate → Opus packets.
#[cfg_attr(faderframe_check_only, allow(dead_code))]
pub struct StreamEncoder {
    #[cfg(not(faderframe_check_only))]
    encoder: opus::Encoder,
    to: Option<[Resampler; 2]>,
    /// 48 kHz samples waiting for a frame, per side.
    acc: [Vec<f32>; 2],
    /// Resampler output (a piece's worth).
    scratch: Vec<f32>,
    interleaved: Vec<f32>,
    packet: Vec<u8>,
}

impl StreamEncoder {
    /// For a mix at `rate`, `kbps` for both sides.
    pub fn new(rate: u32, kbps: u32) -> Result<Self, CodecError> {
        let plan = RatePlan::new(rate)?;
        #[cfg(faderframe_check_only)]
        {
            let _ = (plan, kbps);
            Err(CodecError::Unavailable)
        }
        #[cfg(not(faderframe_check_only))]
        {
            let mut encoder =
                opus::Encoder::new(CODEC_RATE, opus::Channels::Stereo, opus::Application::Audio)
                    .map_err(err)?;
            encoder
                .set_bitrate(opus::Bitrate::Bits((clamp_kbps(kbps) * 1000) as i32))
                .map_err(err)?;
            encoder.set_complexity(8).map_err(err)?;
            let to = plan
                .to_codec()
                .and_then(|l| plan.to_codec().map(|r| [l, r]));
            Ok(Self {
                encoder,
                to,
                acc: [Vec::new(), Vec::new()],
                scratch: Vec::new(),
                interleaved: vec![0.0; FRAME * 2],
                packet: vec![0; MAX_PACKET],
            })
        }
    }

    /// Take `left`/`right` (alike in length); every whole frame becomes a
    /// packet pushed to `out`.
    pub fn push(
        &mut self,
        left: &[f32],
        right: &[f32],
        out: &mut Vec<Vec<u8>>,
    ) -> Result<(), CodecError> {
        let n = left.len().min(right.len());
        for (side, input) in [&left[..n], &right[..n]].into_iter().enumerate() {
            match self.to.as_mut() {
                Some(r) => {
                    self.scratch.resize(input.len() * 8 + 64, 0.0);
                    let made = r[side].process(input, &mut self.scratch);
                    self.acc[side].extend_from_slice(&self.scratch[..made]);
                }
                None => self.acc[side].extend_from_slice(input),
            }
        }
        while self.acc[0].len() >= FRAME && self.acc[1].len() >= FRAME {
            for i in 0..FRAME {
                self.interleaved[2 * i] = self.acc[0][i];
                self.interleaved[2 * i + 1] = self.acc[1][i];
            }
            self.acc[0].drain(..FRAME);
            self.acc[1].drain(..FRAME);
            #[cfg(not(faderframe_check_only))]
            {
                let len = self
                    .encoder
                    .encode_float(&self.interleaved, &mut self.packet)
                    .map_err(err)?;
                out.push(self.packet[..len].to_vec());
            }
            #[cfg(faderframe_check_only)]
            let _ = &out;
        }
        Ok(())
    }
}

/// Opus packets → stereo at the listener's rate.
#[cfg_attr(faderframe_check_only, allow(dead_code))]
pub struct StreamDecoder {
    #[cfg(not(faderframe_check_only))]
    decoder: opus::Decoder,
    from: Option<[Resampler; 2]>,
    pcm: Vec<f32>,
    side: [Vec<f32>; 2],
    scratch: Vec<f32>,
}

impl StreamDecoder {
    /// For a listener at `rate`.
    pub fn new(rate: u32) -> Result<Self, CodecError> {
        let plan = RatePlan::new(rate)?;
        #[cfg(faderframe_check_only)]
        {
            let _ = plan;
            Err(CodecError::Unavailable)
        }
        #[cfg(not(faderframe_check_only))]
        {
            let decoder = opus::Decoder::new(CODEC_RATE, opus::Channels::Stereo).map_err(err)?;
            let from = plan
                .from_codec()
                .and_then(|l| plan.from_codec().map(|r| [l, r]));
            Ok(Self {
                decoder,
                from,
                pcm: vec![0.0; FRAME * 2 * 6],
                side: [Vec::new(), Vec::new()],
                scratch: Vec::new(),
            })
        }
    }

    /// One packet (`None`: one was lost — concealed) appended to `left`
    /// and `right`; returns the frames added.
    pub fn decode(
        &mut self,
        packet: Option<&[u8]>,
        left: &mut Vec<f32>,
        right: &mut Vec<f32>,
    ) -> Result<usize, CodecError> {
        #[cfg(faderframe_check_only)]
        {
            let _ = (packet, left, right);
            Err(CodecError::Unavailable)
        }
        #[cfg(not(faderframe_check_only))]
        {
            // A lost packet is concealed for a frame (the output's length
            // says how much).
            let room = if packet.is_some() {
                self.pcm.len()
            } else {
                FRAME * 2
            };
            let n = self
                .decoder
                .decode_float(packet.unwrap_or(&[]), &mut self.pcm[..room], false)
                .map_err(err)?;
            for (s, v) in self.side.iter_mut().enumerate() {
                v.clear();
                v.extend((0..n).map(|i| self.pcm[2 * i + s]));
            }
            let before = left.len();
            for (s, out) in [&mut *left, &mut *right].into_iter().enumerate() {
                match self.from.as_mut() {
                    Some(r) => {
                        self.scratch.resize(n * 20 + 64, 0.0);
                        let made = r[s].process(&self.side[s], &mut self.scratch);
                        out.extend_from_slice(&self.scratch[..made]);
                    }
                    None => out.extend_from_slice(&self.side[s]),
                }
            }
            Ok(left.len() - before)
        }
    }
}

#[cfg(all(test, not(faderframe_check_only)))]
mod tests {
    use super::*;

    fn tone(rate: u32, hz: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (i as f32 * hz * std::f32::consts::TAU / rate as f32).sin() * 0.5)
            .collect()
    }

    fn level(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    #[test]
    fn the_mix_crosses_as_packets_and_sounds_alike() {
        for (from, to) in [(48_000, 48_000), (44_100, 48_000), (48_000, 96_000)] {
            let mut enc = StreamEncoder::new(from, STREAM_KBPS).unwrap();
            let mut dec = StreamDecoder::new(to).unwrap();
            let l = tone(from, 440.0, from as usize);
            let r = tone(from, 660.0, from as usize);
            let mut packets = Vec::new();
            for (a, b) in l.chunks(333).zip(r.chunks(333)) {
                enc.push(a, b, &mut packets).unwrap();
            }
            // A packet per 20 ms (less the resampler's delay).
            assert!(
                (48..=50).contains(&packets.len()),
                "{from} Hz: {}",
                packets.len()
            );
            assert!(packets.iter().all(|p| p.len() < 1000));
            let (mut ol, mut or) = (Vec::new(), Vec::new());
            for p in &packets {
                dec.decode(Some(p), &mut ol, &mut or).unwrap();
            }
            // A lost packet is concealed, not silence.
            dec.decode(None, &mut ol, &mut or).unwrap();
            let expect = (packets.len() + 1) * FRAME * to as usize / 48_000;
            assert!(
                ol.len().abs_diff(expect) < 200,
                "{from}→{to}: {} vs {expect}",
                ol.len()
            );
            let (a, b) = (
                &ol[to as usize / 4..to as usize * 3 / 4],
                &or[to as usize / 4..to as usize * 3 / 4],
            );
            assert!(
                (level(a) - 0.354).abs() < 0.05,
                "{from}→{to}: left {}",
                level(a)
            );
            assert!(
                (level(b) - 0.354).abs() < 0.05,
                "{from}→{to}: right {}",
                level(b)
            );
        }
    }
}
