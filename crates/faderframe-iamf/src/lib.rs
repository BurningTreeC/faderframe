//! IAMF masters: the Alliance for Open Media's Immersive Audio Model and
//! Formats, as an IA Sequence of OBUs (`.iamf`) or an ISO-BMFF track
//! (`.mp4`, sample entry `iamf`).
//!
//! What FaderFrame writes stays inside the Simple profile of IAMF
//! v1.0.0-errata, so every decoder reads it: one channel-based Audio
//! Element of one layer (Mono, Stereo, 5.1, 5.1.2, 5.1.4, 7.1, 7.1.2 or
//! 7.1.4; at most 16 channels), its channels coded as mono and coupled
//! stereo substreams in the specification's order (§ 3.6.2.3) with LPCM
//! (`ipcm`), FLAC (`fLaC`, independent stereo) or Opus (48 kHz), and one Mix
//! Presentation with one sub-mix carrying the integrated loudness, sample
//! and true peak measured on Stereo (required) and on the authored layout.
//! No parameter blocks: the mix gains are their defaults (0 dB); the last
//! frame is padded and trimmed (Opus' pre-skip is trimmed at the start).
//!
//! The patents necessary for IAMF are licensed by AOM's members royalty
//! free (`AOM-PATENT-LICENSE.txt` in the repository's root).

#![forbid(unsafe_code)]

mod encode;
pub mod mp4;
mod obu;
pub mod read;

pub use encode::SubstreamEncoder;

use obu::{Bits, kind, obu, q7_8};

#[derive(Debug, thiserror::Error)]
pub enum IamfError {
    #[error("IAMF: {0}")]
    Invalid(String),
    #[error("IAMF FLAC: {0}")]
    Flac(String),
    #[error("IAMF Opus: {0}")]
    Opus(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// An IAMF loudspeaker layout (`loudspeaker_layout`, § 3.6.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Layout {
    Mono,
    Stereo,
    S51,
    S512,
    S514,
    S71,
    S712,
    S714,
}

impl Layout {
    pub const ALL: [Layout; 8] = [
        Layout::Mono,
        Layout::Stereo,
        Layout::S51,
        Layout::S512,
        Layout::S514,
        Layout::S71,
        Layout::S712,
        Layout::S714,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Layout::Mono => "Mono",
            Layout::Stereo => "Stereo",
            Layout::S51 => "5.1",
            Layout::S512 => "5.1.2",
            Layout::S514 => "5.1.4",
            Layout::S71 => "7.1",
            Layout::S712 => "7.1.2",
            Layout::S714 => "7.1.4",
        }
    }

    fn code(self) -> u8 {
        match self {
            Layout::Mono => 0,
            Layout::Stereo => 1,
            Layout::S51 => 2,
            Layout::S512 => 3,
            Layout::S514 => 4,
            Layout::S71 => 5,
            Layout::S712 => 6,
            Layout::S714 => 7,
        }
    }

    /// Its loudness layout's `sound_system` (§ 3.7.5).
    pub fn sound_system(self) -> u8 {
        match self {
            Layout::Mono => 12,
            Layout::Stereo => 0,
            Layout::S51 => 1,
            Layout::S512 => 2,
            Layout::S514 => 3,
            Layout::S71 => 8,
            Layout::S712 => 10,
            Layout::S714 => 9,
        }
    }

    /// Its substreams in the specification's order: coupled pairs first
    /// (front, sides, rears, top fronts, top backs), then the centre, then
    /// the LFE. Labels as IAMF names its loudspeakers.
    pub fn substreams(self) -> &'static [&'static [&'static str]] {
        const LR: &[&str] = &["L", "R"];
        const LS: &[&str] = &["Ls", "Rs"];
        const SS: &[&str] = &["Lss", "Rss"];
        const RS: &[&str] = &["Lrs", "Rrs"];
        const TF: &[&str] = &["Ltf", "Rtf"];
        const TR: &[&str] = &["Ltr", "Rtr"];
        const TB: &[&str] = &["Ltb", "Rtb"];
        const C: &[&str] = &["C"];
        const LFE: &[&str] = &["LFE"];
        match self {
            Layout::Mono => &[C],
            Layout::Stereo => &[LR],
            Layout::S51 => &[LR, LS, C, LFE],
            Layout::S512 => &[LR, LS, TF, C, LFE],
            Layout::S514 => &[LR, LS, TF, TR, C, LFE],
            Layout::S71 => &[LR, SS, RS, C, LFE],
            Layout::S712 => &[LR, SS, RS, TF, C, LFE],
            Layout::S714 => &[LR, SS, RS, TF, TB, C, LFE],
        }
    }

    /// Every channel in substream order.
    pub fn channels(self) -> Vec<&'static str> {
        self.substreams()
            .iter()
            .flat_map(|s| s.iter().copied())
            .collect()
    }

    fn coupled(self) -> usize {
        self.substreams().iter().filter(|s| s.len() == 2).count()
    }
}

/// How the substreams are coded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Codec {
    /// Linear PCM, little endian, 16, 24 or 32 bits.
    Lpcm { bits: u8 },
    /// FLAC, 16 or 24 bits.
    Flac { bits: u8 },
    /// Opus at 48 kHz: bits per second for a coupled (stereo) substream,
    /// half that for a mono one.
    Opus { stereo_bitrate: u32 },
}

impl Codec {
    pub fn name(self) -> &'static str {
        match self {
            Codec::Lpcm { .. } => "LPCM",
            Codec::Flac { .. } => "FLAC",
            Codec::Opus { .. } => "Opus",
        }
    }

    fn id(self) -> &'static [u8; 4] {
        match self {
            Codec::Lpcm { .. } => b"ipcm",
            Codec::Flac { .. } => b"fLaC",
            Codec::Opus { .. } => b"Opus",
        }
    }

    /// The rates it can carry, best first for `wanted`.
    pub fn rate_for(self, wanted: u32) -> u32 {
        match self {
            Codec::Opus { .. } => 48_000,
            Codec::Lpcm { .. } => {
                if [16_000, 32_000, 44_100, 48_000, 96_000].contains(&wanted) {
                    wanted
                } else if wanted > 48_000 {
                    96_000
                } else {
                    48_000
                }
            }
            Codec::Flac { .. } => {
                let common = [
                    8_000, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000, 88_200, 96_000, 176_400,
                    192_000,
                ];
                if common.contains(&wanted) {
                    wanted
                } else {
                    48_000
                }
            }
        }
    }

    /// Samples per frame.
    pub fn frame_size(self) -> u32 {
        match self {
            Codec::Lpcm { .. } => 1024,
            Codec::Flac { .. } => 1024,
            Codec::Opus { .. } => 960,
        }
    }

    /// The `codecs` string of an MP4 track (§ 6.4), Simple profile.
    pub fn codecs_string(self) -> String {
        format!(
            "iamf.000.000.{}",
            std::str::from_utf8(self.id()).unwrap_or("ipcm")
        )
    }
}

/// One loudness measurement of the mix (§ 3.7.6): on `layout` (rendered
/// there), integrated loudness (LKFS), sample and true peak (dBFS/dBTP).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Loudness {
    pub layout: Layout,
    pub integrated: f64,
    pub digital_peak: f64,
    pub true_peak: f64,
}

/// What an IA Sequence's descriptors say.
#[derive(Clone, Debug, PartialEq)]
pub struct Master {
    pub layout: Layout,
    pub codec: Codec,
    pub sample_rate: u32,
    /// For the decoder configuration: Opus' pre-skip (samples trimmed at
    /// the start).
    pub pre_skip: u16,
    /// The mix's name (`mix_presentation_friendly_label`).
    pub label: String,
    /// Stereo first; at most one per layout.
    pub loudness: Vec<Loudness>,
}

pub const CODEC_CONFIG_ID: u32 = 0;
pub const AUDIO_ELEMENT_ID: u32 = 10;
pub const MIX_PRESENTATION_ID: u32 = 20;
const ELEMENT_GAIN_ID: u32 = 100;
const OUTPUT_GAIN_ID: u32 = 101;

impl Master {
    pub fn validate(&self) -> Result<(), IamfError> {
        let bad = |s: &str| Err(IamfError::Invalid(s.into()));
        if self.codec.rate_for(self.sample_rate) != self.sample_rate {
            return bad("the codec does not carry this sample rate");
        }
        match self.codec {
            Codec::Lpcm { bits } if ![16, 24, 32].contains(&bits) => {
                return bad("LPCM takes 16, 24 or 32 bits");
            }
            Codec::Flac { bits } if ![16, 24].contains(&bits) => {
                return bad("FLAC takes 16 or 24 bits");
            }
            _ => {}
        }
        if !self.loudness.iter().any(|l| l.layout == Layout::Stereo) {
            return bad("the loudness on Stereo is required");
        }
        Ok(())
    }

    /// The decoder configuration (§ 3.13).
    fn decoder_config(&self) -> Vec<u8> {
        let mut b = Bits::new();
        match self.codec {
            Codec::Lpcm { bits } => {
                // Little endian.
                b.u8(1);
                b.u8(bits);
                b.u32(self.sample_rate);
            }
            Codec::Flac { bits } => {
                // One metadata block: STREAMINFO, the last.
                b.put(1, 1);
                b.put(0, 7);
                b.put(34, 24);
                let n = self.codec.frame_size();
                b.u16(n as u16);
                b.u16(n as u16);
                b.put(0, 24);
                b.put(0, 24);
                b.put(self.sample_rate, 20);
                // Channels − 1 (the frames say mono or stereo).
                b.put(1, 3);
                b.put(u32::from(bits) - 1, 5);
                // Total samples unknown, no MD5.
                b.put(0, 4);
                b.u32(0);
                b.bytes(&[0; 16]);
            }
            Codec::Opus { .. } => {
                // RFC 7845's ID header without the magic, big endian.
                b.u8(1);
                b.u8(2);
                b.u16(self.pre_skip);
                b.u32(48_000);
                b.i16(0);
                b.u8(0);
            }
        }
        b.finish()
    }

    /// The descriptor OBUs: sequence header, codec config, audio element,
    /// mix presentation.
    pub fn descriptors(&self) -> Vec<u8> {
        let mut out = Vec::new();
        // IA Sequence Header: Simple profile.
        obu(&mut out, kind::SEQUENCE_HEADER, b"iamf\x00\x00", None);
        // Codec Config.
        let mut b = Bits::new();
        b.leb128(CODEC_CONFIG_ID);
        b.bytes(self.codec.id());
        b.leb128(self.codec.frame_size());
        let roll = match self.codec {
            Codec::Opus { .. } => -(3840u32.div_ceil(self.codec.frame_size()) as i16),
            _ => 0,
        };
        b.i16(roll);
        b.bytes(&self.decoder_config());
        obu(&mut out, kind::CODEC_CONFIG, &b.finish(), None);
        // Audio Element: channel based, one layer.
        let subs = self.layout.substreams().len() as u32;
        let mut b = Bits::new();
        b.leb128(AUDIO_ELEMENT_ID);
        b.put(0, 3);
        b.put(0, 5);
        b.leb128(CODEC_CONFIG_ID);
        b.leb128(subs);
        for id in 0..subs {
            b.leb128(id);
        }
        b.leb128(0);
        b.put(1, 3);
        b.put(0, 5);
        b.put(u32::from(self.layout.code()), 4);
        b.put(0, 4);
        b.u8(subs as u8);
        b.u8(self.layout.coupled() as u8);
        obu(&mut out, kind::AUDIO_ELEMENT, &b.finish(), None);
        // Mix Presentation.
        let mut b = Bits::new();
        b.leb128(MIX_PRESENTATION_ID);
        b.leb128(1);
        b.string("en-us");
        b.string(&self.label);
        b.leb128(1);
        b.leb128(1);
        b.leb128(AUDIO_ELEMENT_ID);
        b.string("Main");
        // Headphones: a bed binaurally (world-locked), stereo and mono as
        // they are.
        let binaural = !matches!(self.layout, Layout::Mono | Layout::Stereo);
        b.put(u32::from(binaural), 2);
        b.put(0, 6);
        b.leb128(0);
        let gain = |b: &mut Bits, id: u32| {
            b.leb128(id);
            b.leb128(self.sample_rate);
            // Durations in the parameter blocks (there are none).
            b.put(1, 1);
            b.put(0, 7);
            b.i16(0);
        };
        gain(&mut b, ELEMENT_GAIN_ID);
        gain(&mut b, OUTPUT_GAIN_ID);
        b.leb128(self.loudness.len() as u32);
        for l in &self.loudness {
            // Loudspeakers by the sound system convention.
            b.put(2, 2);
            b.put(u32::from(l.layout.sound_system()), 4);
            b.put(0, 2);
            // With the true peak.
            b.u8(1);
            b.i16(q7_8(l.integrated));
            b.i16(q7_8(l.digital_peak));
            b.i16(q7_8(l.true_peak));
        }
        obu(&mut out, kind::MIX_PRESENTATION, &b.finish(), None);
        out
    }
}

/// One Temporal Unit: an Audio Frame OBU per substream (in order), all
/// trimmed alike (`trim`: samples at the start, at the end).
pub fn temporal_unit(frames: &[Vec<u8>], trim: (u32, u32)) -> Vec<u8> {
    let mut out = Vec::new();
    let trim = (trim != (0, 0)).then_some(trim);
    for (id, f) in frames.iter().enumerate() {
        debug_assert!(id < 18, "implicit substream ids");
        obu(&mut out, kind::AUDIO_FRAME_ID0 + id as u8, f, trim);
    }
    out
}

/// A Temporal Delimiter OBU (in a standalone sequence, before each unit).
pub fn temporal_delimiter() -> Vec<u8> {
    let mut out = Vec::new();
    obu(&mut out, kind::TEMPORAL_DELIMITER, &[], None);
    out
}

/// How the sequence is stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
    /// A standalone IA Sequence (`.iamf`), Temporal Delimiters included.
    Raw,
    /// An MP4 file with one IAMF track.
    Mp4,
}

impl Container {
    /// By the file's extension: `.iamf` raw, anything else MP4.
    pub fn for_path(path: &std::path::Path) -> Container {
        match path.extension().and_then(|e| e.to_str()) {
            Some(e) if e.eq_ignore_ascii_case("iamf") => Container::Raw,
            _ => Container::Mp4,
        }
    }
}

/// Code `audio` (one slice per channel, in [`Layout::channels`] order, at
/// `master.sample_rate`) and write it to `path`; `progress` hears the
/// frames done. Fills in `master.pre_skip`.
pub fn write_file(
    path: &std::path::Path,
    master: &mut Master,
    audio: &[&[f32]],
    container: Container,
    mut progress: impl FnMut(u64),
) -> Result<(), IamfError> {
    let layout = master.layout;
    if audio.len() != layout.channels().len() {
        return Err(IamfError::Invalid(format!(
            "{} channels for {}",
            audio.len(),
            layout.name()
        )));
    }
    let len = audio.first().map_or(0, |c| c.len());
    if len == 0 {
        return Err(IamfError::Invalid("nothing to write".into()));
    }
    let n = master.codec.frame_size() as usize;
    let mut encoders = layout
        .substreams()
        .iter()
        .enumerate()
        .map(|(i, s)| SubstreamEncoder::new(master.codec, s.len(), master.sample_rate, i as u32))
        .collect::<Result<Vec<_>, _>>()?;
    let pre_skip = match encoders.first_mut() {
        Some(e) => e.pre_skip()?,
        None => 0,
    } as usize;
    master.pre_skip = pre_skip as u16;
    master.validate()?;
    let total = (len + pre_skip).div_ceil(n) * n;
    let trim_end = total - len - pre_skip;
    let frames = total / n;
    let descriptors = master.descriptors();
    let file = std::fs::File::create(path)?;
    let mut raw = None;
    let mut mp4 = None;
    match container {
        Container::Raw => {
            let mut w = std::io::BufWriter::new(file);
            std::io::Write::write_all(&mut w, &descriptors)?;
            raw = Some(w);
        }
        Container::Mp4 => mp4 = Some(mp4::Mp4Writer::create(std::io::BufWriter::new(file))?),
    }
    // Channels by substream.
    let mut at = 0;
    let groups: Vec<std::ops::Range<usize>> = layout
        .substreams()
        .iter()
        .map(|s| {
            let r = at..at + s.len();
            at += s.len();
            r
        })
        .collect();
    let mut frame_buf: Vec<Vec<f32>> = vec![vec![0.0; n]; audio.len()];
    for f in 0..frames {
        let start = f * n;
        for (c, buf) in frame_buf.iter_mut().enumerate() {
            for (i, v) in buf.iter_mut().enumerate() {
                *v = audio[c].get(start + i).copied().unwrap_or(0.0);
            }
        }
        let coded = groups
            .iter()
            .zip(encoders.iter_mut())
            .map(|(g, e)| {
                let chans: Vec<&[f32]> = frame_buf[g.clone()].iter().map(Vec::as_slice).collect();
                e.encode(&chans)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let trim = (
            if f == 0 { pre_skip as u32 } else { 0 },
            if f + 1 == frames { trim_end as u32 } else { 0 },
        );
        let unit = temporal_unit(&coded, trim);
        if let Some(w) = raw.as_mut() {
            std::io::Write::write_all(w, &temporal_delimiter())?;
            std::io::Write::write_all(w, &unit)?;
        }
        if let Some(m) = mp4.as_mut() {
            m.sample(&unit, (n as u32) - trim.1)?;
        }
        progress((start + n).min(len) as u64);
    }
    if let Some(mut w) = raw {
        std::io::Write::flush(&mut w)?;
    }
    if let Some(m) = mp4 {
        m.finish(&mp4::Track {
            descriptors: &descriptors,
            sample_rate: master.sample_rate,
            trim_start: pre_skip as u32,
            presented: len as u64,
            roll: match master.codec {
                Codec::Opus { .. } => Some(-(3840u32.div_ceil(n as u32) as i16)),
                _ => None,
            },
        })?;
    }
    Ok(())
}

/// The stereo IAMF decoders render a layout's channels (in
/// [`Layout::channels`] order) to: § 10.1.2.2's surround chain with the
/// default demixing mode 1 (α = β = 1, δ = 0.707; the centre at 0.707),
/// the top fronts at unity and the top backs at γ = 0.707 (T4to2), the LFE
/// dropped — as AOM's reference decoder (libiamf) renders it, measured.
/// The stereo loudness is measured on this.
pub fn stereo_downmix(layout: Layout, channels: &[&[f32]]) -> [Vec<f32>; 2] {
    let names = layout.channels();
    let frames = channels.first().map_or(0, |c| c.len());
    let get = |n: &str| {
        names
            .iter()
            .position(|x| *x == n)
            .and_then(|i| channels.get(i))
    };
    let (mut l, mut r) = (vec![0.0f32; frames], vec![0.0f32; frames]);
    let mut add = |name: &str, gl: f32, gr: f32| {
        if let Some(c) = get(name) {
            for i in 0..frames {
                l[i] += gl * c[i];
                r[i] += gr * c[i];
            }
        }
    };
    let d = std::f32::consts::FRAC_1_SQRT_2;
    match layout {
        Layout::Mono => {
            // § 3.7: L = R = 0.707 × Mono.
            add("C", d, d);
        }
        _ => {
            add("L", 1.0, 0.0);
            add("R", 0.0, 1.0);
            add("C", d, d);
            // 7 → 5 (α = β = 1), 5 → 3 (δ); the tops: fronts at unity,
            // backs at γ.
            for (name, g) in [
                ("Ls", d),
                ("Lss", d),
                ("Lrs", d),
                ("Ltf", 1.0),
                ("Ltr", d),
                ("Ltb", d),
            ] {
                add(name, g, 0.0);
                let right = name.replacen('L', "R", 1);
                add(&right, 0.0, g);
            }
        }
    }
    [l, r]
}

/// BS.1770 weights of a layout's channels (0 for the LFE, 1.41 for those
/// at the sides and behind at ear level).
pub fn loudness_weights(layout: Layout) -> Vec<f64> {
    layout
        .channels()
        .iter()
        .map(|c| match *c {
            "LFE" => 0.0,
            "Ls" | "Rs" | "Lss" | "Rss" | "Lrs" | "Rrs" => std::f64::consts::SQRT_2,
            _ => 1.0,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn master(codec: Codec, layout: Layout) -> Master {
        Master {
            layout,
            codec,
            sample_rate: codec.rate_for(48_000),
            pre_skip: if matches!(codec, Codec::Opus { .. }) {
                312
            } else {
                0
            },
            label: "Song".into(),
            loudness: vec![
                Loudness {
                    layout: Layout::Stereo,
                    integrated: -16.0,
                    digital_peak: -1.5,
                    true_peak: -1.0,
                },
                Loudness {
                    layout,
                    integrated: -18.25,
                    digital_peak: -2.0,
                    true_peak: -1.75,
                },
            ],
        }
    }

    #[test]
    fn layouts_list_their_substreams_in_the_specification_s_order() {
        assert_eq!(
            Layout::S714.channels(),
            [
                "L", "R", "Lss", "Rss", "Lrs", "Rrs", "Ltf", "Rtf", "Ltb", "Rtb", "C", "LFE"
            ]
        );
        assert_eq!(Layout::S714.substreams().len(), 7);
        assert_eq!(Layout::S714.coupled(), 5);
        assert_eq!(Layout::S51.channels(), ["L", "R", "Ls", "Rs", "C", "LFE"]);
        for l in Layout::ALL {
            assert!(l.channels().len() <= 16, "Simple profile");
        }
    }

    /// The descriptors read back as written.
    #[test]
    fn descriptors_read_back() {
        for codec in [
            Codec::Lpcm { bits: 24 },
            Codec::Flac { bits: 24 },
            Codec::Opus {
                stereo_bitrate: 128_000,
            },
        ] {
            let m = master(codec, Layout::S714);
            m.validate().unwrap();
            let bytes = m.descriptors();
            let seq = read::parse(&bytes).unwrap();
            assert_eq!(seq.profile, (0, 0));
            assert_eq!(seq.codec_id, *codec.id());
            assert_eq!(seq.frame_size, codec.frame_size());
            assert_eq!(seq.layout, 7);
            assert_eq!((seq.substreams, seq.coupled), (7, 5));
            assert_eq!(seq.label, "Song");
            assert_eq!(seq.loudness.len(), 2);
            assert_eq!(seq.loudness[0], (0, -16 * 256, -384, -256));
            assert_eq!(seq.loudness[1].0, 9);
            assert_eq!(seq.loudness[1].1, q7_8(-18.25));
            if let Codec::Opus { .. } = codec {
                assert_eq!(seq.roll, -4);
                assert_eq!(&seq.decoder_config[2..4], &312u16.to_be_bytes());
            }
        }
    }

    #[test]
    fn a_stereo_loudness_is_required_and_rates_are_checked() {
        let mut m = master(Codec::Lpcm { bits: 24 }, Layout::S51);
        m.loudness.remove(0);
        assert!(m.validate().is_err());
        let mut m = master(Codec::Opus { stereo_bitrate: 1 }, Layout::S51);
        m.sample_rate = 44_100;
        assert!(m.validate().is_err());
        assert_eq!(Codec::Lpcm { bits: 24 }.rate_for(88_200), 96_000);
        assert_eq!(Codec::Flac { bits: 24 }.rate_for(88_200), 88_200);
    }

    #[test]
    fn the_stereo_downmix_follows_the_specification() {
        let one = vec![1.0f32; 4];
        let zero = vec![0.0f32; 4];
        let gain = |index: usize| {
            let mut ch: Vec<&[f32]> = vec![&zero; 12];
            ch[index] = &one;
            let [l, r] = stereo_downmix(Layout::S714, &ch);
            (l[0], r[0])
        };
        let d = std::f32::consts::FRAC_1_SQRT_2;
        // As libiamf renders it: L, Lss, Ltf, Rtb, C, LFE.
        assert_eq!(gain(0), (1.0, 0.0));
        assert_eq!(gain(2), (d, 0.0));
        assert_eq!(gain(6), (1.0, 0.0));
        assert_eq!(gain(9), (0.0, d), "Rtb");
        assert_eq!(gain(10), (d, d), "C");
        assert_eq!(gain(11), (0.0, 0.0), "LFE");
        let [l, r] = stereo_downmix(Layout::Mono, &[&one]);
        assert_eq!(l, r);
        assert_eq!(
            loudness_weights(Layout::S51),
            [1.0, 1.0, 2f64.sqrt(), 2f64.sqrt(), 1.0, 0.0]
        );
    }
}
