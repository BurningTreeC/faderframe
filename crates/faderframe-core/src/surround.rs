//! Surround beds: channel-based speaker formats and the panner that places
//! sources in them.
//!
//! A format's channels are in the order WAVE files carry them (the bits of
//! their channel mask, as ALSA and HDMI order them too), so files and
//! device outputs need no reordering. Speakers sit in a room box: `x` from
//! −1 (left) to 1 (right), `y` from −1 (back) to 1 (front), `z` from 0
//! (ear level) to 1 (ceiling); LFE has no place.
//!
//! The panner ([`SurroundPan`], [`point_gains`]) works as console and
//! Dolby bed panners do in such a box: power-preserving crossfades between
//! the floor and top layers (by `z`), between the rows of a layer (by `y`)
//! and between the speakers of a row (by `x`); `spread` blends towards
//! every speaker of the format alike. A stereo source's channels sit
//! `width` apart; the LFE is a separate send. A bed into another format
//! ([`matrix`]) places each of its speakers where it stands (folding 7.1.4
//! into 5.1 or stereo, or opening 5.1 up); into the same format it passes
//! straight through. Nothing here allocates: strips evaluate it on the
//! audio thread.

use crate::ChannelLayout;
use serde::{Deserialize, Serialize};

/// Most channels a format has (7.1.4).
pub const MAX_SPEAKERS: usize = 12;

/// One loudspeaker of a format.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Speaker {
    pub label: &'static str,
    /// Its bit in a WAVE channel mask.
    pub mask: u32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub lfe: bool,
}

const fn spk(label: &'static str, mask: u32, x: f32, y: f32, z: f32) -> Speaker {
    Speaker {
        label,
        mask,
        x,
        y,
        z,
        lfe: false,
    }
}

const L: Speaker = spk("L", 0x1, -1.0, 1.0, 0.0);
const R: Speaker = spk("R", 0x2, 1.0, 1.0, 0.0);
const C: Speaker = spk("C", 0x4, 0.0, 1.0, 0.0);
const LFE: Speaker = Speaker {
    label: "LFE",
    mask: 0x8,
    x: 0.0,
    y: 0.0,
    z: 0.0,
    lfe: true,
};
/// Rear (back) surrounds.
const LRS: Speaker = spk("Lrs", 0x10, -1.0, -1.0, 0.0);
const RRS: Speaker = spk("Rrs", 0x20, 1.0, -1.0, 0.0);
/// Side surrounds (7.x).
const LSS: Speaker = spk("Lss", 0x200, -1.0, 0.0, 0.0);
const RSS: Speaker = spk("Rss", 0x400, 1.0, 0.0, 0.0);
/// 5.x surrounds (behind the listener, carried as side channels).
const LS: Speaker = spk("Ls", 0x200, -1.0, -1.0, 0.0);
const RS: Speaker = spk("Rs", 0x400, 1.0, -1.0, 0.0);
/// Quad's rear pair.
const LQ: Speaker = spk("Ls", 0x10, -1.0, -1.0, 0.0);
const RQ: Speaker = spk("Rs", 0x20, 1.0, -1.0, 0.0);
/// Heights: front and rear tops, and top middles (x.1.2; a WAVE mask has
/// no bits of their own, the top front ones stand in).
const LTF: Speaker = spk("Ltf", 0x1000, -1.0, 1.0, 1.0);
const RTF: Speaker = spk("Rtf", 0x4000, 1.0, 1.0, 1.0);
const LTR: Speaker = spk("Ltr", 0x8000, -1.0, -1.0, 1.0);
const RTR: Speaker = spk("Rtr", 0x20000, 1.0, -1.0, 1.0);
const LTM: Speaker = spk("Ltm", 0x1000, -1.0, 0.0, 1.0);
const RTM: Speaker = spk("Rtm", 0x4000, 1.0, 0.0, 1.0);

/// Stereo as the panner sees it (folding beds into it).
const STEREO: [Speaker; 2] = [L, R];
const MONO: [Speaker; 1] = [spk("M", 0x4, 0.0, 1.0, 0.0)];

/// A channel-based speaker format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurroundFormat {
    /// Left, right, centre.
    Lcr,
    Quad,
    #[serde(rename = "5.0")]
    S50,
    #[serde(rename = "5.1")]
    S51,
    #[serde(rename = "7.0")]
    S70,
    #[serde(rename = "7.1")]
    S71,
    #[serde(rename = "5.1.2")]
    S512,
    #[serde(rename = "5.1.4")]
    S514,
    #[serde(rename = "7.1.2")]
    S712,
    #[serde(rename = "7.1.4")]
    S714,
}

impl SurroundFormat {
    pub const ALL: [SurroundFormat; 10] = [
        SurroundFormat::Lcr,
        SurroundFormat::Quad,
        SurroundFormat::S50,
        SurroundFormat::S51,
        SurroundFormat::S70,
        SurroundFormat::S71,
        SurroundFormat::S512,
        SurroundFormat::S514,
        SurroundFormat::S712,
        SurroundFormat::S714,
    ];

    /// Its speakers, in channel order.
    pub const fn speakers(self) -> &'static [Speaker] {
        match self {
            SurroundFormat::Lcr => &[L, R, C],
            SurroundFormat::Quad => &[L, R, LQ, RQ],
            SurroundFormat::S50 => &[L, R, C, LS, RS],
            SurroundFormat::S51 => &[L, R, C, LFE, LS, RS],
            SurroundFormat::S70 => &[L, R, C, LRS, RRS, LSS, RSS],
            SurroundFormat::S71 => &[L, R, C, LFE, LRS, RRS, LSS, RSS],
            SurroundFormat::S512 => &[L, R, C, LFE, LS, RS, LTM, RTM],
            SurroundFormat::S514 => &[L, R, C, LFE, LS, RS, LTF, RTF, LTR, RTR],
            SurroundFormat::S712 => &[L, R, C, LFE, LRS, RRS, LSS, RSS, LTM, RTM],
            SurroundFormat::S714 => &[L, R, C, LFE, LRS, RRS, LSS, RSS, LTF, RTF, LTR, RTR],
        }
    }

    pub const fn channels(self) -> usize {
        self.speakers().len()
    }

    pub fn name(self) -> &'static str {
        match self {
            SurroundFormat::Lcr => "LCR",
            SurroundFormat::Quad => "Quad",
            SurroundFormat::S50 => "5.0",
            SurroundFormat::S51 => "5.1",
            SurroundFormat::S70 => "7.0",
            SurroundFormat::S71 => "7.1",
            SurroundFormat::S512 => "5.1.2",
            SurroundFormat::S514 => "5.1.4",
            SurroundFormat::S712 => "7.1.2",
            SurroundFormat::S714 => "7.1.4",
        }
    }

    /// The WAVE channel mask.
    pub fn channel_mask(self) -> u32 {
        self.speakers().iter().fold(0, |m, s| m | s.mask)
    }

    pub fn has_lfe(self) -> bool {
        self.speakers().iter().any(|s| s.lfe)
    }

    pub fn has_heights(self) -> bool {
        self.speakers().iter().any(|s| s.z > 0.0)
    }
}

/// The speakers a layout plays on (mono and stereo as the panner sees
/// them; `None` for discrete channels).
pub fn speakers_of(layout: ChannelLayout) -> Option<&'static [Speaker]> {
    match layout {
        ChannelLayout::Mono => Some(&MONO),
        ChannelLayout::Stereo => Some(&STEREO),
        ChannelLayout::Surround(f) => Some(f.speakers()),
        ChannelLayout::Discrete(_) => None,
    }
}

fn is_default_width(v: &f32) -> bool {
    *v == 1.0
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

fn front() -> f32 {
    1.0
}

fn is_front(v: &f32) -> bool {
    *v == 1.0
}

fn no_lfe() -> f32 {
    LFE_OFF_DB
}

fn is_no_lfe(v: &f32) -> bool {
    *v <= LFE_OFF_DB
}

/// An LFE send at or below this is off.
pub const LFE_OFF_DB: f32 = -100.0;

/// Where a track sits in a surround bed.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SurroundPan {
    /// −1 left … 1 right.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub x: f32,
    /// −1 back … 1 front.
    #[serde(default = "front", skip_serializing_if = "is_front")]
    pub y: f32,
    /// 0 ear level … 1 ceiling (formats with heights).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub z: f32,
    /// 0 a point … 1 every speaker alike.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub spread: f32,
    /// A stereo source's channels apart (0 together … 1 left and right
    /// of the position).
    #[serde(default = "one", skip_serializing_if = "is_default_width")]
    pub width: f32,
    /// The LFE send (dB; [`LFE_OFF_DB`] and below: off).
    #[serde(default = "no_lfe", skip_serializing_if = "is_no_lfe")]
    pub lfe_db: f32,
}

fn one() -> f32 {
    1.0
}

impl Default for SurroundPan {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 1.0,
            z: 0.0,
            spread: 0.0,
            width: 1.0,
            lfe_db: LFE_OFF_DB,
        }
    }
}

impl SurroundPan {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Clamped to its ranges.
    pub fn clamped(self) -> Self {
        Self {
            x: self.x.clamp(-1.0, 1.0),
            y: self.y.clamp(-1.0, 1.0),
            z: self.z.clamp(0.0, 1.0),
            spread: self.spread.clamp(0.0, 1.0),
            width: self.width.clamp(0.0, 1.0),
            lfe_db: self.lfe_db.clamp(LFE_OFF_DB, 12.0),
        }
    }

    pub fn lfe_gain(&self) -> f32 {
        if self.lfe_db <= LFE_OFF_DB {
            0.0
        } else {
            crate::db_to_gain(self.lfe_db)
        }
    }
}

/// What a bed `layout` going to `room` device outputs is folded down to
/// (`None`: it fits, or it is not a bed): the largest format that fits,
/// else stereo, else mono.
pub fn fold_into(layout: ChannelLayout, room: usize) -> Option<ChannelLayout> {
    let ChannelLayout::Surround(_) = layout else {
        return None;
    };
    if layout.channel_count() <= room {
        return None;
    }
    SurroundFormat::ALL
        .iter()
        .filter(|f| f.channels() <= room)
        .max_by_key(|f| f.channels())
        .map(|f| ChannelLayout::Surround(*f))
        .or(Some(if room >= 2 {
            ChannelLayout::Stereo
        } else {
            ChannelLayout::Mono
        }))
}

/// One of a [`SurroundPan`]'s values (what an automation lane moves; the
/// order of the strip's surround slots).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurroundParam {
    X,
    Y,
    Z,
    Spread,
    Width,
    Lfe,
}

impl SurroundParam {
    pub const ALL: [SurroundParam; 6] = [
        SurroundParam::X,
        SurroundParam::Y,
        SurroundParam::Z,
        SurroundParam::Spread,
        SurroundParam::Width,
        SurroundParam::Lfe,
    ];

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn name(self) -> &'static str {
        match self {
            SurroundParam::X => "Surround Left/Right",
            SurroundParam::Y => "Surround Front/Back",
            SurroundParam::Z => "Surround Height",
            SurroundParam::Spread => "Surround Spread",
            SurroundParam::Width => "Surround Width",
            SurroundParam::Lfe => "LFE Send",
        }
    }

    /// `(min, max, default)`.
    pub fn range(self) -> (f32, f32, f32) {
        match self {
            SurroundParam::X => (-1.0, 1.0, 0.0),
            SurroundParam::Y => (-1.0, 1.0, 1.0),
            SurroundParam::Z | SurroundParam::Spread => (0.0, 1.0, 0.0),
            SurroundParam::Width => (0.0, 1.0, 1.0),
            SurroundParam::Lfe => (LFE_OFF_DB, 12.0, LFE_OFF_DB),
        }
    }

    pub fn get(self, pan: &SurroundPan) -> f32 {
        match self {
            SurroundParam::X => pan.x,
            SurroundParam::Y => pan.y,
            SurroundParam::Z => pan.z,
            SurroundParam::Spread => pan.spread,
            SurroundParam::Width => pan.width,
            SurroundParam::Lfe => pan.lfe_db,
        }
    }

    /// `pan` with this value set to `v` (clamped).
    pub fn set(self, mut pan: SurroundPan, v: f32) -> SurroundPan {
        let (lo, hi, default) = self.range();
        let v = if v.is_nan() { default } else { v.clamp(lo, hi) };
        match self {
            SurroundParam::X => pan.x = v,
            SurroundParam::Y => pan.y = v,
            SurroundParam::Z => pan.z = v,
            SurroundParam::Spread => pan.spread = v,
            SurroundParam::Width => pan.width = v,
            SurroundParam::Lfe => pan.lfe_db = v,
        }
        pan
    }

    /// Whether it does anything for a `source` panned into `format`.
    pub fn applies(self, source: ChannelLayout, format: SurroundFormat) -> bool {
        match self {
            SurroundParam::Z => format.has_heights(),
            SurroundParam::Width => source.channel_count() == 2,
            SurroundParam::Lfe => format.has_lfe(),
            _ => true,
        }
    }
}

/// Power-preserving crossfade between two neighbours at `t` (0 the
/// first, 1 the second).
fn crossfade(t: f32) -> (f32, f32) {
    let a = t.clamp(0.0, 1.0) * std::f32::consts::FRAC_PI_2;
    (a.cos(), a.sin())
}

/// The neighbours bracketing `v` among `values` (the nearest below and
/// above; the outermost when `v` lies beyond them), and the crossfade.
fn bracket(v: f32, values: impl Iterator<Item = f32> + Clone) -> Option<(f32, f32, f32, f32)> {
    let below = values
        .clone()
        .filter(|&p| p <= v)
        .fold(None, |m: Option<f32>, p| Some(m.map_or(p, |m| m.max(p))));
    let above = values
        .clone()
        .filter(|&p| p >= v)
        .fold(None, |m: Option<f32>, p| Some(m.map_or(p, |m| m.min(p))));
    match (below, above) {
        (Some(a), Some(b)) if (b - a).abs() > 1e-6 => {
            let (ga, gb) = crossfade((v - a) / (b - a));
            Some((a, ga, b, gb))
        }
        (Some(a), _) | (None, Some(a)) => Some((a, 1.0, a, 0.0)),
        (None, None) => None,
    }
}

/// The gain of `speakers[i]` for a point source at (`x`, `y`, `z`) with
/// `spread` (into `out`, one per speaker; LFE stays 0). The squares sum to
/// one.
pub fn point_gains(speakers: &[Speaker], x: f32, y: f32, z: f32, spread: f32, out: &mut [f32]) {
    for o in out.iter_mut() {
        *o = 0.0;
    }
    let mains = || speakers.iter().filter(|s| !s.lfe);
    let tops = mains().any(|s| s.z > 0.0);
    let (floor_w, top_w) = if tops { crossfade(z) } else { (1.0, 0.0) };
    for (layer_z, layer_w) in [(0.0f32, floor_w), (1.0, top_w)] {
        if layer_w <= 0.0 {
            continue;
        }
        let in_layer = |s: &&Speaker| (s.z > 0.5) == (layer_z > 0.5);
        let Some((ya, ga, yb, gb)) = bracket(y, mains().filter(in_layer).map(|s| s.y)) else {
            continue;
        };
        for (row_y, row_w) in [(ya, ga), (yb, gb)] {
            if row_w <= 0.0 {
                continue;
            }
            let in_row = |s: &&Speaker| in_layer(s) && (s.y - row_y).abs() < 1e-6;
            let Some((xa, ha, xb, hb)) = bracket(x, mains().filter(in_row).map(|s| s.x)) else {
                continue;
            };
            for (i, s) in speakers.iter().enumerate() {
                if s.lfe || !in_row(&s) || i >= out.len() {
                    continue;
                }
                let col = if (s.x - xa).abs() < 1e-6 {
                    ha
                } else if (s.x - xb).abs() < 1e-6 {
                    hb
                } else {
                    0.0
                };
                out[i] += layer_w * row_w * col;
            }
        }
    }
    let spread = spread.clamp(0.0, 1.0);
    if spread > 0.0 {
        let n = mains().count().max(1) as f32;
        for (o, s) in out.iter_mut().zip(speakers) {
            if !s.lfe {
                *o = ((1.0 - spread) * *o * *o + spread / n).sqrt();
            }
        }
    }
}

/// Gains from each channel of `src` (rows) to each channel of `dst`
/// (columns), placed by `pan` (for mono, stereo and discrete sources; a bed
/// keeps its own speakers' places). Returns whether the matrix applies
/// (`false`: the layouts pass channel by channel as before, e.g. stereo to
/// stereo).
pub fn matrix(
    src: ChannelLayout,
    dst: ChannelLayout,
    pan: &SurroundPan,
    out: &mut [[f32; MAX_SPEAKERS]; MAX_SPEAKERS],
) -> bool {
    let surround = |l: ChannelLayout| matches!(l, ChannelLayout::Surround(_));
    if !surround(src) && !surround(dst) {
        return false;
    }
    for row in out.iter_mut() {
        row.fill(0.0);
    }
    let Some(to) = speakers_of(dst) else {
        // Into discrete channels: one to one.
        for (c, row) in out
            .iter_mut()
            .enumerate()
            .take(src.channel_count().min(dst.channel_count()))
        {
            row[c] = 1.0;
        }
        return true;
    };
    if src == dst {
        for (c, row) in out.iter_mut().enumerate().take(to.len()) {
            row[c] = 1.0;
        }
        return true;
    }
    let lfe_out = to.iter().position(|s| s.lfe);
    let pan = pan.clamped();
    match speakers_of(src) {
        // A bed (or stereo, mono) into another format: each speaker where
        // it stands; LFE to LFE.
        Some(from) if surround(src) => {
            for (row, s) in out.iter_mut().zip(from) {
                if s.lfe {
                    if let Some(l) = lfe_out {
                        row[l] = 1.0;
                    }
                } else {
                    point_gains(to, s.x, s.y, s.z, 0.0, row);
                }
            }
        }
        // Mono, stereo or discrete into a bed: placed by the panner.
        _ => {
            let n = src.channel_count().min(MAX_SPEAKERS);
            for (c, row) in out.iter_mut().enumerate().take(n) {
                let x = if src == ChannelLayout::Stereo {
                    pan.x + if c == 0 { -pan.width } else { pan.width }
                } else {
                    pan.x
                };
                point_gains(to, x.clamp(-1.0, 1.0), pan.y, pan.z, pan.spread, row);
                if let Some(l) = lfe_out {
                    row[l] = pan.lfe_gain() / n as f32;
                }
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn power(g: &[f32]) -> f32 {
        g.iter().map(|v| v * v).sum()
    }

    fn gains(f: SurroundFormat, x: f32, y: f32, z: f32, spread: f32) -> Vec<f32> {
        let mut g = vec![0.0; f.channels()];
        point_gains(f.speakers(), x, y, z, spread, &mut g);
        g
    }

    #[test]
    fn formats_are_in_wave_order_with_their_masks() {
        assert_eq!(SurroundFormat::S51.channel_mask(), 0x60F);
        assert_eq!(SurroundFormat::S71.channel_mask(), 0x63F);
        assert_eq!(SurroundFormat::S714.channel_mask(), 0x2D63F);
        assert_eq!(SurroundFormat::S714.channels(), 12);
        for f in SurroundFormat::ALL {
            let masks: Vec<u32> = f.speakers().iter().map(|s| s.mask).collect();
            let mut sorted = masks.clone();
            sorted.sort_unstable();
            assert_eq!(masks, sorted, "{} in mask order", f.name());
        }
    }

    #[test]
    fn a_point_lands_on_its_speakers_and_keeps_its_power() {
        let f = SurroundFormat::S714;
        let at = |label: &str| f.speakers().iter().position(|s| s.label == label).unwrap();
        // Front centre: the C speaker alone.
        let g = gains(f, 0.0, 1.0, 0.0, 0.0);
        assert!((g[at("C")] - 1.0).abs() < 1e-6, "{g:?}");
        // Halfway between L and C.
        let g = gains(f, -0.5, 1.0, 0.0, 0.0);
        assert!((g[at("L")] - g[at("C")]).abs() < 1e-6);
        // Left side: Lss.
        let g = gains(f, -1.0, 0.0, 0.0, 0.0);
        assert!((g[at("Lss")] - 1.0).abs() < 1e-6);
        // Up: the top layer only; the LFE never.
        let g = gains(f, -1.0, -1.0, 1.0, 0.0);
        assert!((g[at("Ltr")] - 1.0).abs() < 1e-6);
        assert_eq!(g[at("LFE")], 0.0);
        for f in SurroundFormat::ALL {
            for (x, y, z, s) in [
                (0.3, -0.2, 0.4, 0.0),
                (-1.0, 1.0, 0.0, 0.5),
                (0.9, -0.9, 1.0, 1.0),
                (0.0, 0.0, 0.0, 0.0),
            ] {
                let p = power(&gains(f, x, y, z, s));
                assert!((p - 1.0).abs() < 1e-4, "{} at {x},{y},{z}: {p}", f.name());
            }
        }
        // Full spread: every main speaker alike.
        let g = gains(SurroundFormat::S51, 0.0, 1.0, 0.0, 1.0);
        let mains: Vec<f32> = g.iter().copied().filter(|v| *v > 0.0).collect();
        assert_eq!(mains.len(), 5);
        assert!(mains.iter().all(|v| (v - mains[0]).abs() < 1e-6));
    }

    #[test]
    fn stereo_sources_spread_by_width_and_beds_fold_by_place() {
        let s51 = ChannelLayout::Surround(SurroundFormat::S51);
        let mut m = [[0.0; MAX_SPEAKERS]; MAX_SPEAKERS];
        let pan = SurroundPan {
            lfe_db: 0.0,
            ..SurroundPan::default()
        };
        assert!(matrix(ChannelLayout::Stereo, s51, &pan, &mut m));
        // Left input on L, right on R, both half into the LFE.
        assert!((m[0][0] - 1.0).abs() < 1e-6 && (m[1][1] - 1.0).abs() < 1e-6);
        assert!((m[0][3] - 0.5).abs() < 1e-6);
        // Stereo to stereo stays the strip's own (balance).
        assert!(!matrix(
            ChannelLayout::Stereo,
            ChannelLayout::Stereo,
            &pan,
            &mut m
        ));
        // 5.1 into stereo: C at −3 dB in each, surrounds to their side,
        // the LFE dropped.
        assert!(matrix(s51, ChannelLayout::Stereo, &pan, &mut m));
        assert!((m[2][0] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        assert!((m[4][0] - 1.0).abs() < 1e-6 && m[4][1] == 0.0);
        assert_eq!(m[3], [0.0; MAX_SPEAKERS]);
        // 7.1.4 into 5.1: the side surrounds between the front and the
        // surrounds, the heights down onto the floor, LFE to LFE.
        let s714 = ChannelLayout::Surround(SurroundFormat::S714);
        assert!(matrix(s714, s51, &pan, &mut m));
        assert!((m[3][3] - 1.0).abs() < 1e-6);
        assert!(m[6][0] > 0.6 && m[6][4] > 0.6, "Lss: {:?}", &m[6][..6]);
        assert!(
            (power(&m[8][..6]) - 1.0).abs() < 1e-4,
            "Ltf keeps its power"
        );
        // The same format: straight through.
        assert!(matrix(s51, s51, &pan, &mut m));
        assert_eq!(m[5][5], 1.0);
    }
}
