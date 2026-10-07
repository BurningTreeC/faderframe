//! Reading back what [`crate`] writes (the subset it uses): tests and
//! checks of written files.

use crate::IamfError;
use crate::obu::kind;

/// An IA Sequence as read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sequence {
    pub profile: (u8, u8),
    pub codec_id: [u8; 4],
    pub frame_size: u32,
    pub roll: i16,
    pub decoder_config: Vec<u8>,
    /// `loudspeaker_layout` of the (one) layer.
    pub layout: u8,
    pub substreams: u8,
    pub coupled: u8,
    pub label: String,
    pub headphones_mode: u8,
    /// (sound system, integrated, digital peak, true peak), Q7.8.
    pub loudness: Vec<(u8, i16, i16, i16)>,
    /// Audio frames: (substream, trim at start, trim at end, payload).
    pub frames: Vec<(u8, u32, u32, Vec<u8>)>,
    pub delimiters: usize,
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

fn short() -> IamfError {
    IamfError::Invalid("truncated".into())
}

impl Reader<'_> {
    fn u8(&mut self) -> Result<u8, IamfError> {
        let v = *self.b.get(self.at).ok_or_else(short)?;
        self.at += 1;
        Ok(v)
    }

    fn u16(&mut self) -> Result<u16, IamfError> {
        Ok(u16::from_be_bytes([self.u8()?, self.u8()?]))
    }

    fn bytes(&mut self, n: usize) -> Result<&[u8], IamfError> {
        let s = self.b.get(self.at..self.at + n).ok_or_else(short)?;
        self.at += n;
        Ok(s)
    }

    fn leb128(&mut self) -> Result<u32, IamfError> {
        let mut v = 0u64;
        for i in 0..8 {
            let byte = self.u8()?;
            v |= u64::from(byte & 0x7F) << (7 * i);
            if byte & 0x80 == 0 {
                return u32::try_from(v).map_err(|_| IamfError::Invalid("leb128".into()));
            }
        }
        Err(IamfError::Invalid("leb128".into()))
    }

    fn string(&mut self) -> Result<String, IamfError> {
        let start = self.at;
        while self.u8()? != 0 {}
        Ok(String::from_utf8_lossy(&self.b[start..self.at - 1]).into_owned())
    }

    /// A MixGainParamDefinition.
    fn mix_gain(&mut self) -> Result<(), IamfError> {
        self.leb128()?;
        self.leb128()?;
        let mode = self.u8()? >> 7;
        if mode == 0 {
            self.leb128()?;
            let constant = self.leb128()?;
            if constant == 0 {
                for _ in 0..self.leb128()? {
                    self.leb128()?;
                }
            }
        }
        self.u16()?;
        Ok(())
    }
}

/// Parse an IA Sequence.
pub fn parse(bytes: &[u8]) -> Result<Sequence, IamfError> {
    let mut s = Sequence::default();
    let mut r = Reader { b: bytes, at: 0 };
    while r.at < bytes.len() {
        let head = r.u8()?;
        let kind = head >> 3;
        let trimmed = head & 0b10 != 0;
        let size = r.leb128()? as usize;
        let end = r.at + size;
        let (mut t_start, mut t_end) = (0, 0);
        if trimmed && (5..24).contains(&kind) {
            t_end = r.leb128()?;
            t_start = r.leb128()?;
        }
        match kind {
            kind::SEQUENCE_HEADER => {
                if r.bytes(4)? != b"iamf" {
                    return Err(IamfError::Invalid("no ia_code".into()));
                }
                s.profile = (r.u8()?, r.u8()?);
            }
            kind::CODEC_CONFIG => {
                r.leb128()?;
                s.codec_id.copy_from_slice(r.bytes(4)?);
                s.frame_size = r.leb128()?;
                s.roll = r.u16()? as i16;
                s.decoder_config = r.bytes(end - r.at)?.to_vec();
            }
            kind::AUDIO_ELEMENT => {
                r.leb128()?;
                r.u8()?;
                r.leb128()?;
                for _ in 0..r.leb128()? {
                    r.leb128()?;
                }
                if r.leb128()? != 0 {
                    return Err(IamfError::Invalid("parameters".into()));
                }
                if r.u8()? >> 5 != 1 {
                    return Err(IamfError::Invalid("layers".into()));
                }
                s.layout = r.u8()? >> 4;
                s.substreams = r.u8()?;
                s.coupled = r.u8()?;
            }
            kind::MIX_PRESENTATION => {
                r.leb128()?;
                let labels = r.leb128()?;
                for _ in 0..labels {
                    r.string()?;
                }
                for i in 0..labels {
                    let l = r.string()?;
                    if i == 0 {
                        s.label = l;
                    }
                }
                for _ in 0..r.leb128()? {
                    for _ in 0..r.leb128()? {
                        r.leb128()?;
                        for _ in 0..labels {
                            r.string()?;
                        }
                        s.headphones_mode = r.u8()? >> 6;
                        let ext = r.leb128()? as usize;
                        r.bytes(ext)?;
                        r.mix_gain()?;
                    }
                    r.mix_gain()?;
                    for _ in 0..r.leb128()? {
                        let layout = r.u8()?;
                        let info = r.u8()?;
                        let integrated = r.u16()? as i16;
                        let digital = r.u16()? as i16;
                        let tp = if info & 1 != 0 { r.u16()? as i16 } else { 0 };
                        s.loudness
                            .push(((layout >> 2) & 0xF, integrated, digital, tp));
                    }
                }
            }
            kind::TEMPORAL_DELIMITER => s.delimiters += 1,
            k if (6..24).contains(&k) => {
                let payload = r.bytes(end - r.at)?.to_vec();
                s.frames.push((k - 6, t_start, t_end, payload));
            }
            _ => {}
        }
        r.at = end;
    }
    Ok(s)
}
