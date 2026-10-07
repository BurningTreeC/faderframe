//! Coding one substream (mono or coupled stereo) frame by frame.

use crate::{Codec, IamfError};
use flacenc::bitsink::ByteSink;
use flacenc::component::{BitRepr, StreamInfo};
use flacenc::error::Verify;
use flacenc::source::{Fill, FrameBuf};

/// Codes one substream's frames (`Codec::frame_size` samples per channel
/// each).
pub enum SubstreamEncoder {
    Lpcm {
        bits: u8,
        channels: usize,
        dither: Dither,
    },
    Flac {
        config: flacenc::error::Verified<flacenc::config::Encoder>,
        info: StreamInfo,
        buf: FrameBuf,
        frame: usize,
        bits: u8,
        channels: usize,
        dither: Dither,
    },
    #[cfg(not(faderframe_check_only))]
    Opus {
        encoder: Box<opus::Encoder>,
        channels: usize,
        interleaved: Vec<f32>,
        packet: Vec<u8>,
    },
}

/// TPDF dither for 16 bits (a cheap generator per substream).
pub struct Dither(u32);

impl Dither {
    fn next(&mut self) -> f32 {
        // xorshift32
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x as f32 / u32::MAX as f32
    }

    /// `x` as an integer of `bits` (dithered at 16 bits, rounded above).
    fn quantize(&mut self, x: f32, bits: u8) -> i32 {
        let scale = (1i64 << (bits - 1)) as f64;
        let d = if bits <= 16 {
            f64::from(self.next() - self.next())
        } else {
            0.0
        };
        let v = (f64::from(x) * scale + d).round();
        v.clamp(-scale, scale - 1.0) as i32
    }
}

#[cfg(not(faderframe_check_only))]
fn opus_err(e: opus::Error) -> IamfError {
    IamfError::Opus(e.to_string())
}

impl SubstreamEncoder {
    /// For `channels` (1 or 2) at `rate`; `seed` varies the dither between
    /// substreams.
    pub fn new(
        codec: Codec,
        channels: usize,
        rate: u32,
        seed: u32,
    ) -> Result<SubstreamEncoder, IamfError> {
        if !(1..=2).contains(&channels) {
            return Err(IamfError::Invalid("a substream is mono or stereo".into()));
        }
        let dither = Dither(0x9E37_79B9 ^ seed.wrapping_mul(0x85EB_CA6B) | 1);
        Ok(match codec {
            Codec::Lpcm { bits } => SubstreamEncoder::Lpcm {
                bits,
                channels,
                dither,
            },
            Codec::Flac { bits } => {
                let mut c = flacenc::config::Encoder::default();
                // Frames say mono or independent stereo (§ 3.13.3).
                c.stereo_coding.use_leftside = false;
                c.stereo_coding.use_rightside = false;
                c.stereo_coding.use_midside = false;
                c.multithread = false;
                c.block_size = codec.frame_size() as usize;
                let config = c
                    .into_verified()
                    .map_err(|(_, e)| IamfError::Flac(format!("{e:?}")))?;
                let info = StreamInfo::new(rate as usize, channels, usize::from(bits))
                    .map_err(|e| IamfError::Flac(format!("{e:?}")))?;
                let buf = FrameBuf::with_size(channels, codec.frame_size() as usize)
                    .map_err(|e| IamfError::Flac(format!("{e:?}")))?;
                SubstreamEncoder::Flac {
                    config,
                    info,
                    buf,
                    frame: 0,
                    bits,
                    channels,
                    dither,
                }
            }
            #[cfg(faderframe_check_only)]
            Codec::Opus { .. } => {
                return Err(IamfError::Opus("not in this build".into()));
            }
            #[cfg(not(faderframe_check_only))]
            Codec::Opus { stereo_bitrate } => {
                let mut encoder = opus::Encoder::new(
                    48_000,
                    if channels == 2 {
                        opus::Channels::Stereo
                    } else {
                        opus::Channels::Mono
                    },
                    opus::Application::Audio,
                )
                .map_err(opus_err)?;
                let rate = if channels == 2 {
                    stereo_bitrate
                } else {
                    stereo_bitrate / 2
                };
                encoder
                    .set_bitrate(opus::Bitrate::Bits(rate as i32))
                    .map_err(opus_err)?;
                SubstreamEncoder::Opus {
                    encoder: Box::new(encoder),
                    channels,
                    interleaved: vec![0.0; channels * codec.frame_size() as usize],
                    packet: vec![0; 4000],
                }
            }
        })
    }

    /// The samples a decoder discards at the start (Opus' lookahead).
    pub fn pre_skip(&mut self) -> Result<u32, IamfError> {
        match self {
            #[cfg(not(faderframe_check_only))]
            SubstreamEncoder::Opus { encoder, .. } => {
                Ok(encoder.get_lookahead().map_err(opus_err)?.max(0) as u32)
            }
            _ => Ok(0),
        }
    }

    /// One frame: `input` holds each channel's samples (a full frame).
    pub fn encode(&mut self, input: &[&[f32]]) -> Result<Vec<u8>, IamfError> {
        let n = input.first().map_or(0, |c| c.len());
        match self {
            SubstreamEncoder::Lpcm {
                bits,
                channels,
                dither,
            } => {
                let width = usize::from(*bits / 8);
                let mut out = Vec::with_capacity(n * *channels * width);
                for i in 0..n {
                    for c in input.iter().take(*channels) {
                        let v = dither.quantize(c[i], *bits);
                        out.extend_from_slice(&v.to_le_bytes()[..width]);
                    }
                }
                Ok(out)
            }
            SubstreamEncoder::Flac {
                config,
                info,
                buf,
                frame,
                bits,
                channels,
                dither,
            } => {
                let mut interleaved = Vec::with_capacity(n * *channels);
                for i in 0..n {
                    for c in input.iter().take(*channels) {
                        interleaved.push(dither.quantize(c[i], *bits));
                    }
                }
                buf.fill_interleaved(&interleaved)
                    .map_err(|e| IamfError::Flac(format!("{e:?}")))?;
                let f = flacenc::encode_fixed_size_frame(config, buf, *frame, info)
                    .map_err(|e| IamfError::Flac(format!("{e:?}")))?;
                *frame += 1;
                let mut sink = ByteSink::new();
                f.write(&mut sink)
                    .map_err(|e| IamfError::Flac(format!("{e:?}")))?;
                Ok(sink.as_slice().to_vec())
            }
            #[cfg(not(faderframe_check_only))]
            SubstreamEncoder::Opus {
                encoder,
                channels,
                interleaved,
                packet,
            } => {
                interleaved.resize(n * *channels, 0.0);
                for i in 0..n {
                    for (k, c) in input.iter().take(*channels).enumerate() {
                        interleaved[i * *channels + k] = c[i];
                    }
                }
                let len = encoder
                    .encode_float(interleaved, packet)
                    .map_err(opus_err)?;
                Ok(packet[..len].to_vec())
            }
        }
    }
}
