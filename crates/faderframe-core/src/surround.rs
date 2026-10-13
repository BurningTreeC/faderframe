//! Surround beds: channel-based speaker formats and the panner that places
//! sources in them.
//!
//! A format's channels are in the order WAVE files carry them (the bits of
//! their channel mask, as ALSA and HDMI order them too), so files and
//! device outputs need no reordering. Each speaker has an identity (its
//! label and mask; the LFE is flagged and has no place or direction) and
//! two descriptions of where it stands:
//!
//! * a place in a room box, for the room panner: `x` from −1 (left) to 1
//!   (right), `y` from −1 (back) to 1 (front), `z` from 0 (ear level) to 1
//!   (ceiling);
//! * a direction from the listener, for VBAP ([`crate::vbap`]) and the
//!   binaural renderer: azimuth in degrees, 0 straight ahead and positive to
//!   the left (ITU-R BS.2051 / ADM), elevation in degrees above ear level.
//!   The fronts stand at ±30°, 5.x surrounds at ±110°, 7.x sides at ±90°
//!   and backs at ±135°, quad's rear pair at ±135°; tops at ±45° and
//!   ±135°, 30° up (x.1.4), top middles at ±90°, 45° up (x.1.2).
//!
//! Two panners place a source ([`SurroundPan::panner`]): the room panner
//! ([`point_gains`]) works as console and Dolby bed panners do in the
//! box — power-preserving crossfades between the floor and top layers (by
//! `z`), between the rows of a layer (by `y`) and between the speakers of a
//! row (by `x`); VBAP places it by direction (azimuth, elevation) between
//! the two or three speakers around it. Either way `spread` blends towards
//! every speaker of the format alike and the LFE is a separate send that
//! no position ever reaches. A stereo source is placed as an image (its
//! channels `width` apart), balanced on the format's L and R, or each
//! channel on its own ([`StereoSource`]). A bed into another format
//! ([`matrix`]) passes straight through into the same format, folds into
//! stereo or mono by the project's [`Downmix`], and into any other bed
//! places each of its speakers where it stands (7.1.4 into 5.1, or 5.1
//! opened up). Nothing here allocates: strips evaluate it on the audio
//! thread (VBAP's geometry is made beforehand, [`crate::vbap::Vbap::new`]).

use crate::ChannelLayout;
use serde::{Deserialize, Serialize};

/// Most channels a format has (a custom layout's limit; 7.1.4 has 12).
pub const MAX_SPEAKERS: usize = 16;

/// One loudspeaker of a format.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Speaker {
    pub label: &'static str,
    /// Its bit in a WAVE channel mask.
    pub mask: u32,
    /// Its place in the room box (the room panner).
    pub x: f32,
    pub y: f32,
    pub z: f32,
    /// Its direction from the listener in degrees (VBAP, binaural):
    /// azimuth 0 ahead, positive to the left; elevation above ear level.
    pub azimuth: f32,
    pub elevation: f32,
    /// The LFE channel: no place, no direction, never panned to.
    pub lfe: bool,
}

impl Speaker {
    /// Its direction as a unit vector (x right, y front, z up).
    pub fn direction(&self) -> [f32; 3] {
        crate::vbap::unit(self.azimuth, self.elevation)
    }

    /// Whether it is a front, side, rear or top speaker of the left or the
    /// right (by its direction; `None`: on the centre line).
    pub fn side(&self) -> Option<Side> {
        let a = self.azimuth.rem_euclid(360.0);
        if self.lfe || a < 0.5 || (a - 180.0).abs() < 0.5 || a > 359.5 {
            None
        } else if a < 180.0 {
            Some(Side::Left)
        } else {
            Some(Side::Right)
        }
    }

    /// Its role in a downmix (see [`Downmix`]).
    pub fn role(&self) -> Role {
        if self.lfe {
            Role::Lfe
        } else if self.elevation > 0.0 {
            Role::Height
        } else if self.azimuth.abs() < 0.5 {
            Role::Centre
        } else if self.azimuth.abs() <= 45.0 {
            Role::Front
        } else {
            Role::Surround
        }
    }
}

/// Left or right of the centre line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// What a speaker is in a downmix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// L or R (and mono's M).
    Front,
    Centre,
    /// Any ear-level speaker beyond ±45°: sides, 5.x surrounds, backs.
    Surround,
    /// Any speaker above ear level.
    Height,
    Lfe,
}

#[allow(clippy::too_many_arguments)]
const fn spk(
    label: &'static str,
    mask: u32,
    x: f32,
    y: f32,
    z: f32,
    azimuth: f32,
    elevation: f32,
) -> Speaker {
    Speaker {
        label,
        mask,
        x,
        y,
        z,
        azimuth,
        elevation,
        lfe: false,
    }
}

const L: Speaker = spk("L", 0x1, -1.0, 1.0, 0.0, 30.0, 0.0);
const R: Speaker = spk("R", 0x2, 1.0, 1.0, 0.0, -30.0, 0.0);
const C: Speaker = spk("C", 0x4, 0.0, 1.0, 0.0, 0.0, 0.0);
const LFE: Speaker = Speaker {
    label: "LFE",
    mask: 0x8,
    x: 0.0,
    y: 0.0,
    z: 0.0,
    azimuth: 0.0,
    elevation: 0.0,
    lfe: true,
};
/// Rear (back) surrounds.
const LRS: Speaker = spk("Lrs", 0x10, -1.0, -1.0, 0.0, 135.0, 0.0);
const RRS: Speaker = spk("Rrs", 0x20, 1.0, -1.0, 0.0, -135.0, 0.0);
/// Side surrounds (7.x).
const LSS: Speaker = spk("Lss", 0x200, -1.0, 0.0, 0.0, 90.0, 0.0);
const RSS: Speaker = spk("Rss", 0x400, 1.0, 0.0, 0.0, -90.0, 0.0);
/// 5.x surrounds (behind the listener, carried as side channels).
const LS: Speaker = spk("Ls", 0x200, -1.0, -1.0, 0.0, 110.0, 0.0);
const RS: Speaker = spk("Rs", 0x400, 1.0, -1.0, 0.0, -110.0, 0.0);
/// Quad's rear pair.
const LQ: Speaker = spk("Ls", 0x10, -1.0, -1.0, 0.0, 135.0, 0.0);
const RQ: Speaker = spk("Rs", 0x20, 1.0, -1.0, 0.0, -135.0, 0.0);
/// Heights: front and rear tops, and top middles (x.1.2; a WAVE mask has
/// no bits of their own, the top front ones stand in).
const LTF: Speaker = spk("Ltf", 0x1000, -1.0, 1.0, 1.0, 45.0, 30.0);
const RTF: Speaker = spk("Rtf", 0x4000, 1.0, 1.0, 1.0, -45.0, 30.0);
const LTR: Speaker = spk("Ltr", 0x8000, -1.0, -1.0, 1.0, 135.0, 30.0);
const RTR: Speaker = spk("Rtr", 0x20000, 1.0, -1.0, 1.0, -135.0, 30.0);
const LTM: Speaker = spk("Ltm", 0x1000, -1.0, 0.0, 1.0, 90.0, 45.0);
const RTM: Speaker = spk("Rtm", 0x4000, 1.0, 0.0, 1.0, -90.0, 45.0);

/// Stereo as the panner sees it (folding beds into it).
const STEREO: [Speaker; 2] = [L, R];
const MONO: [Speaker; 1] = [spk("M", 0x4, 0.0, 1.0, 0.0, 0.0, 0.0)];

/// A channel-based speaker format: a built-in one, or a layout the user
/// defined ([`LayoutDef`], made a format by [`LayoutDef::intern`]).
#[derive(Clone, Copy, Debug)]
pub enum SurroundFormat {
    /// Left, right, centre.
    Lcr,
    Quad,
    S50,
    S51,
    S70,
    S71,
    S512,
    S514,
    S712,
    S714,
    /// A layout the user defined (interned: one per definition, so formats
    /// compare by it).
    Custom(&'static CustomLayout),
}

impl SurroundFormat {
    /// The built-in formats.
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
            SurroundFormat::Custom(c) => c.speakers,
        }
    }

    pub const fn channels(self) -> usize {
        self.speakers().len()
    }

    /// A layout the user defined.
    pub fn custom(self) -> Option<&'static CustomLayout> {
        match self {
            SurroundFormat::Custom(c) => Some(c),
            _ => None,
        }
    }

    /// How a project file names a built-in format (`None`: a custom one,
    /// written out in full).
    fn id(self) -> Option<&'static str> {
        Some(match self {
            SurroundFormat::Lcr => "lcr",
            SurroundFormat::Quad => "quad",
            SurroundFormat::S50 => "5.0",
            SurroundFormat::S51 => "5.1",
            SurroundFormat::S70 => "7.0",
            SurroundFormat::S71 => "7.1",
            SurroundFormat::S512 => "5.1.2",
            SurroundFormat::S514 => "5.1.4",
            SurroundFormat::S712 => "7.1.2",
            SurroundFormat::S714 => "7.1.4",
            SurroundFormat::Custom(_) => return None,
        })
    }

    /// What tells formats apart: a built-in's place, a custom layout's
    /// address (one per definition).
    fn key(self) -> (usize, usize) {
        match self {
            SurroundFormat::Custom(c) => (usize::MAX, std::ptr::from_ref(c) as usize),
            f => (
                Self::ALL.iter().position(|a| a.id() == f.id()).unwrap_or(0),
                0,
            ),
        }
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
            SurroundFormat::Custom(c) => c.name,
        }
    }

    /// The WAVE channel mask.
    pub fn channel_mask(self) -> u32 {
        self.speakers().iter().fold(0, |m, s| m | s.mask)
    }

    pub fn has_lfe(self) -> bool {
        self.speakers().iter().any(|s| s.lfe)
    }

    /// Whether a speaker stands below ear level (custom layouts).
    pub fn has_lower(self) -> bool {
        self.speakers().iter().any(|s| !s.lfe && s.elevation < 0.0)
    }

    /// Whether a source can be placed up or down (VBAP's elevation).
    pub fn is_3d(self) -> bool {
        self.speakers().iter().any(|s| !s.lfe && s.elevation != 0.0)
    }

    /// The elevations a source can take (degrees): 0…90, down to −90
    /// where speakers stand below ear level.
    pub fn elevation_range(self) -> (f32, f32) {
        (if self.has_lower() { -90.0 } else { 0.0 }, 90.0)
    }

    pub fn has_heights(self) -> bool {
        self.speakers().iter().any(|s| s.z > 0.0)
    }
}

impl PartialEq for SurroundFormat {
    fn eq(&self, other: &Self) -> bool {
        self.key() == other.key()
    }
}

impl Eq for SurroundFormat {}

impl std::hash::Hash for SurroundFormat {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.key().hash(state);
    }
}

impl Serialize for SurroundFormat {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match (self.id(), self.custom()) {
            (Some(id), _) => serializer.serialize_str(id),
            (None, Some(c)) => c.def.serialize(serializer),
            (None, None) => serializer.serialize_str("5.1"),
        }
    }
}

impl<'de> Deserialize<'de> for SurroundFormat {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Written {
            Named(String),
            Custom(LayoutDef),
        }
        match Written::deserialize(deserializer)? {
            Written::Named(id) => SurroundFormat::ALL
                .into_iter()
                .find(|f| f.id() == Some(id.as_str()))
                .ok_or_else(|| serde::de::Error::custom(format!("no speaker format '{id}'"))),
            Written::Custom(def) => def.intern().map_err(serde::de::Error::custom),
        }
    }
}

/// A speaker layout the user defined, as a format plays it (interned by
/// [`LayoutDef::intern`]; lives as long as the program).
#[derive(Debug)]
pub struct CustomLayout {
    pub name: &'static str,
    pub speakers: &'static [Speaker],
    /// What it was made from (written to project files).
    pub def: LayoutDef,
}

/// One speaker of a [`LayoutDef`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpeakerDef {
    pub label: String,
    /// Degrees, 0 ahead, positive to the left (−180…180).
    #[serde(default)]
    pub azimuth: f32,
    /// Degrees above ear level (−90…90).
    #[serde(default)]
    pub elevation: f32,
    /// The LFE channel (no direction).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lfe: bool,
}

/// A speaker layout the user defines: its channels in order (the order
/// device outputs and files carry them), each a direction or the LFE.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayoutDef {
    pub name: String,
    pub speakers: Vec<SpeakerDef>,
}

/// Why a [`LayoutDef`] cannot be a format.
#[derive(Clone, Debug, PartialEq)]
pub enum LayoutError {
    NoName,
    /// The name of a built-in format.
    TakenName(String),
    NoSpeakers,
    TooMany,
    BadLabel(usize),
    SameLabel(usize),
    TwoLfe,
    NoMain,
    BadDirection(usize),
    /// VBAP cannot pan over it.
    Geometry(crate::vbap::VbapError),
}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LayoutError::NoName => write!(f, "the layout needs a name"),
            LayoutError::TakenName(n) => write!(f, "{n} is a built-in format"),
            LayoutError::NoSpeakers => write!(f, "the layout has no speakers"),
            LayoutError::TooMany => write!(f, "at most {MAX_SPEAKERS} speakers"),
            LayoutError::BadLabel(i) => write!(f, "speaker {} needs a label", i + 1),
            LayoutError::SameLabel(i) => write!(f, "speaker {}'s label is taken", i + 1),
            LayoutError::TwoLfe => write!(f, "only one LFE channel"),
            LayoutError::NoMain => write!(f, "the layout needs a speaker besides the LFE"),
            LayoutError::BadDirection(i) => {
                write!(f, "speaker {}: azimuth −180…180°, elevation −90…90°", i + 1)
            }
            LayoutError::Geometry(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for LayoutError {}

/// The interned custom layouts (the control thread only: the audio thread
/// holds the `&'static` it was given).
static CUSTOM: std::sync::Mutex<Vec<&'static CustomLayout>> = std::sync::Mutex::new(Vec::new());

impl LayoutDef {
    /// A speaker as a format holds it: its room place derived from its
    /// direction (on the box's walls; more than 0° up: the top layer).
    fn speaker(d: &SpeakerDef, label: &'static str) -> Speaker {
        if d.lfe {
            return Speaker { label, ..LFE };
        }
        let azimuth = wrap_azimuth(d.azimuth);
        let v = crate::vbap::unit(azimuth, 0.0);
        let k = v[0].abs().max(v[1].abs()).max(1e-6);
        let flat = d.elevation.to_radians().cos();
        Speaker {
            label,
            mask: 0,
            x: (v[0] / k * flat).clamp(-1.0, 1.0),
            y: (v[1] / k * flat).clamp(-1.0, 1.0),
            z: if d.elevation > 0.0 { 1.0 } else { 0.0 },
            azimuth,
            elevation: d.elevation,
            lfe: false,
        }
    }

    /// What is wrong with it, if anything (its geometry included).
    pub fn validate(&self) -> Result<(), LayoutError> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err(LayoutError::NoName);
        }
        if SurroundFormat::ALL
            .iter()
            .any(|f| f.name().eq_ignore_ascii_case(name))
            || ["mono", "stereo"]
                .iter()
                .any(|n| n.eq_ignore_ascii_case(name))
        {
            return Err(LayoutError::TakenName(name.to_string()));
        }
        if self.speakers.is_empty() {
            return Err(LayoutError::NoSpeakers);
        }
        if self.speakers.len() > MAX_SPEAKERS {
            return Err(LayoutError::TooMany);
        }
        for (i, s) in self.speakers.iter().enumerate() {
            let label = s.label.trim();
            if label.is_empty() {
                return Err(LayoutError::BadLabel(i));
            }
            if self.speakers[..i]
                .iter()
                .any(|o| o.label.trim().eq_ignore_ascii_case(label))
            {
                return Err(LayoutError::SameLabel(i));
            }
            if !s.lfe
                && (!s.azimuth.is_finite()
                    || !s.elevation.is_finite()
                    || s.azimuth.abs() > 180.0
                    || s.elevation.abs() > 90.0)
            {
                return Err(LayoutError::BadDirection(i));
            }
        }
        if self.speakers.iter().filter(|s| s.lfe).count() > 1 {
            return Err(LayoutError::TwoLfe);
        }
        if self.speakers.iter().all(|s| s.lfe) {
            return Err(LayoutError::NoMain);
        }
        let speakers: Vec<Speaker> = self.speakers.iter().map(|d| Self::speaker(d, "")).collect();
        crate::vbap::Vbap::new(&speakers).map_err(LayoutError::Geometry)?;
        Ok(())
    }

    /// The format it defines (checked; the same definition is the same
    /// format every time). Control thread.
    pub fn intern(&self) -> Result<SurroundFormat, LayoutError> {
        self.validate()?;
        let mut all = CUSTOM
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(c) = all.iter().find(|c| c.def == *self) {
            return Ok(SurroundFormat::Custom(c));
        }
        let leak = |s: &str| -> &'static str { Box::leak(s.trim().to_string().into_boxed_str()) };
        let speakers: Vec<Speaker> = self
            .speakers
            .iter()
            .map(|d| Self::speaker(d, leak(&d.label)))
            .collect();
        let layout: &'static CustomLayout = Box::leak(Box::new(CustomLayout {
            name: leak(&self.name),
            speakers: Box::leak(speakers.into_boxed_slice()),
            def: self.clone(),
        }));
        all.push(layout);
        Ok(SurroundFormat::Custom(layout))
    }

    /// A built-in format's speakers as a definition to start from.
    pub fn from_format(format: SurroundFormat, name: &str) -> Self {
        Self {
            name: name.to_string(),
            speakers: format
                .speakers()
                .iter()
                .map(|s| SpeakerDef {
                    label: s.label.to_string(),
                    azimuth: s.azimuth,
                    elevation: s.elevation,
                    lfe: s.lfe,
                })
                .collect(),
        }
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

fn left_front() -> f32 {
    30.0
}

fn is_left_front(v: &f32) -> bool {
    *v == 30.0
}

fn right_front() -> f32 {
    -30.0
}

fn is_right_front(v: &f32) -> bool {
    *v == -30.0
}

/// How a source's position becomes speaker gains.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurroundPanner {
    /// The room box (`x`, `y`, `z`): crossfades between layers, rows and
    /// speakers, as console and Dolby bed panners work ([`point_gains`]).
    #[default]
    Room,
    /// By direction (`azimuth`, `elevation`) between the speakers around
    /// it ([`crate::vbap`]).
    Vbap,
}

impl SurroundPanner {
    pub const ALL: [SurroundPanner; 2] = [SurroundPanner::Room, SurroundPanner::Vbap];

    pub fn label(self) -> &'static str {
        match self {
            SurroundPanner::Room => "Room",
            SurroundPanner::Vbap => "VBAP",
        }
    }

    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// How a stereo source goes into a bed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StereoSource {
    /// Placed as an image: its channels `width` apart either side of the
    /// position (room: `x ∓ width`; VBAP: `azimuth ± 30° × width`).
    #[default]
    Image,
    /// Not placed: left on the format's L, right on its R, at the strip's
    /// balance (unity at centre); no spread.
    Balance,
    /// Each channel at its own direction (`left`, `right`; VBAP).
    Dual,
}

impl StereoSource {
    pub const ALL: [StereoSource; 3] = [
        StereoSource::Image,
        StereoSource::Balance,
        StereoSource::Dual,
    ];

    pub fn label(self) -> &'static str {
        match self {
            StereoSource::Image => "Image",
            StereoSource::Balance => "Balance",
            StereoSource::Dual => "Dual",
        }
    }

    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

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
    /// Which panner places it: the room (`x`, `y`, `z`) or VBAP
    /// (`azimuth`, `elevation`). Each keeps its own values.
    #[serde(default, skip_serializing_if = "SurroundPanner::is_default")]
    pub panner: SurroundPanner,
    /// VBAP: the direction in degrees, 0 ahead, positive to the left
    /// (−180…180).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub azimuth: f32,
    /// VBAP: degrees above ear level (0…90; formats with heights).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub elevation: f32,
    /// How a stereo source goes in.
    #[serde(default, skip_serializing_if = "StereoSource::is_default")]
    pub stereo: StereoSource,
    /// Dual (VBAP): the left channel's azimuth (default the L speaker's).
    #[serde(default = "left_front", skip_serializing_if = "is_left_front")]
    pub left: f32,
    /// Dual (VBAP): the right channel's azimuth.
    #[serde(default = "right_front", skip_serializing_if = "is_right_front")]
    pub right: f32,
    /// Spread diffuse rather than coherent: the spread's share reaches
    /// every speaker through its own decorrelation filter
    /// ([`diffuse_gain`]) instead of as the same signal.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub diffuse: bool,
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
            panner: SurroundPanner::Room,
            azimuth: 0.0,
            elevation: 0.0,
            stereo: StereoSource::Image,
            left: 30.0,
            right: -30.0,
            diffuse: false,
        }
    }
}

/// An azimuth in −180…180 (NaN: ahead).
fn wrap_azimuth(a: f32) -> f32 {
    if !a.is_finite() {
        return 0.0;
    }
    let w = (a + 180.0).rem_euclid(360.0) - 180.0;
    // −180 and 180 are the same direction: keep the one given.
    if w == -180.0 && a > 0.0 { 180.0 } else { w }
}

/// How many values carry a [`SurroundPan`] ([`SurroundPan::values`]): every
/// [`SurroundParam`], then the panner, the stereo mode and diffuse spread.
pub const SURROUND_VALUES: usize = SurroundParam::ALL.len() + 3;

impl SurroundPan {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Clamped to its ranges (a NaN back to its default).
    pub fn clamped(self) -> Self {
        SurroundParam::ALL
            .iter()
            .fold(self, |pan, p| p.set(pan, p.get(&self)))
    }

    /// Its values as numbers (the engine's parameter slots carry them):
    /// each [`SurroundParam`] at its index, then the panner and the stereo
    /// mode.
    pub fn values(&self) -> [f32; SURROUND_VALUES] {
        let mut v = [0.0; SURROUND_VALUES];
        for p in SurroundParam::ALL {
            v[p.index()] = p.get(self);
        }
        let n = SurroundParam::ALL.len();
        v[n] = match self.panner {
            SurroundPanner::Room => 0.0,
            SurroundPanner::Vbap => 1.0,
        };
        v[n + 1] = match self.stereo {
            StereoSource::Image => 0.0,
            StereoSource::Balance => 1.0,
            StereoSource::Dual => 2.0,
        };
        v[n + 2] = if self.diffuse { 1.0 } else { 0.0 };
        v
    }

    /// From [`Self::values`] (clamped; unknown modes are the defaults).
    pub fn from_values(v: &[f32; SURROUND_VALUES]) -> Self {
        let n = SurroundParam::ALL.len();
        let base = Self {
            panner: if v[n].round() == 1.0 {
                SurroundPanner::Vbap
            } else {
                SurroundPanner::Room
            },
            stereo: match v[n + 1].round() {
                1.0 => StereoSource::Balance,
                2.0 => StereoSource::Dual,
                _ => StereoSource::Image,
            },
            diffuse: v[n + 2] >= 0.5,
            ..Self::default()
        };
        SurroundParam::ALL
            .iter()
            .fold(base, |pan, p| p.set(pan, v[p.index()]))
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
    Azimuth,
    Elevation,
    /// Dual: the left channel's azimuth.
    LeftAzimuth,
    /// Dual: the right channel's azimuth.
    RightAzimuth,
}

impl SurroundParam {
    pub const ALL: [SurroundParam; 10] = [
        SurroundParam::X,
        SurroundParam::Y,
        SurroundParam::Z,
        SurroundParam::Spread,
        SurroundParam::Width,
        SurroundParam::Lfe,
        SurroundParam::Azimuth,
        SurroundParam::Elevation,
        SurroundParam::LeftAzimuth,
        SurroundParam::RightAzimuth,
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
            SurroundParam::Azimuth => "Surround Azimuth",
            SurroundParam::Elevation => "Surround Elevation",
            SurroundParam::LeftAzimuth => "Left Channel Azimuth",
            SurroundParam::RightAzimuth => "Right Channel Azimuth",
        }
    }

    /// Its unit for readouts ("" for the room's −1…1 and 0…1 values).
    pub fn unit(self) -> &'static str {
        match self {
            SurroundParam::Lfe => "dB",
            SurroundParam::Azimuth
            | SurroundParam::Elevation
            | SurroundParam::LeftAzimuth
            | SurroundParam::RightAzimuth => "°",
            _ => "",
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
            SurroundParam::Azimuth => (-180.0, 180.0, 0.0),
            // Stored over the whole sphere; a format offers its own part
            // (`range_in`).
            SurroundParam::Elevation => (-90.0, 90.0, 0.0),
            SurroundParam::LeftAzimuth => (-180.0, 180.0, 30.0),
            SurroundParam::RightAzimuth => (-180.0, 180.0, -30.0),
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
            SurroundParam::Azimuth => pan.azimuth,
            SurroundParam::Elevation => pan.elevation,
            SurroundParam::LeftAzimuth => pan.left,
            SurroundParam::RightAzimuth => pan.right,
        }
    }

    /// `(min, max, default)` for a source panned into `format`: elevation
    /// only down to ear level unless speakers stand below it.
    pub fn range_in(self, format: SurroundFormat) -> (f32, f32, f32) {
        match self {
            SurroundParam::Elevation => {
                let (lo, hi) = format.elevation_range();
                (lo, hi, 0.0)
            }
            p => p.range(),
        }
    }

    /// Whether it is a direction in degrees that goes round (azimuths:
    /// set wraps them into −180…180 instead of clamping).
    pub fn wraps(self) -> bool {
        matches!(
            self,
            SurroundParam::Azimuth | SurroundParam::LeftAzimuth | SurroundParam::RightAzimuth
        )
    }

    /// `pan` with this value set to `v` (clamped; azimuths wrapped; NaN:
    /// the default).
    pub fn set(self, mut pan: SurroundPan, v: f32) -> SurroundPan {
        let (lo, hi, default) = self.range();
        let v = if v.is_nan() {
            default
        } else if self.wraps() {
            wrap_azimuth(v)
        } else {
            v.clamp(lo, hi)
        };
        match self {
            SurroundParam::X => pan.x = v,
            SurroundParam::Y => pan.y = v,
            SurroundParam::Z => pan.z = v,
            SurroundParam::Spread => pan.spread = v,
            SurroundParam::Width => pan.width = v,
            SurroundParam::Lfe => pan.lfe_db = v,
            SurroundParam::Azimuth => pan.azimuth = v,
            SurroundParam::Elevation => pan.elevation = v,
            SurroundParam::LeftAzimuth => pan.left = v,
            SurroundParam::RightAzimuth => pan.right = v,
        }
        pan
    }

    /// Whether it does anything for a `source` panned into `format` the
    /// way `pan` says (its panner and, for stereo, its mode).
    pub fn applies(self, source: ChannelLayout, format: SurroundFormat, pan: &SurroundPan) -> bool {
        let stereo = source.channel_count() == 2;
        let mode = if stereo {
            pan.stereo
        } else {
            StereoSource::Image
        };
        let mode = match (mode, pan.panner) {
            (StereoSource::Dual, SurroundPanner::Room) => StereoSource::Image,
            (m, _) => m,
        };
        let placed = mode != StereoSource::Balance;
        let room = pan.panner == SurroundPanner::Room;
        match self {
            SurroundParam::X | SurroundParam::Y => placed && room,
            SurroundParam::Z => placed && room && format.has_heights(),
            SurroundParam::Azimuth => mode == StereoSource::Image && !room,
            SurroundParam::Elevation => placed && !room && format.is_3d(),
            SurroundParam::LeftAzimuth | SurroundParam::RightAzimuth => mode == StereoSource::Dual,
            SurroundParam::Spread => placed,
            SurroundParam::Width => stereo && mode == StereoSource::Image,
            SurroundParam::Lfe => format.has_lfe(),
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
    spread_over(speakers, spread, out);
}

/// `spread` (0…1) of a point's gains blended towards every main speaker
/// alike, power for power: `g'ᵢ = √((1 − s)·gᵢ² + s/n)` over the `n` main
/// speakers (the squares still sum to one). The same signal reaches more
/// speakers — a wider, coherent image, not a decorrelated diffuse field.
pub fn spread_over(speakers: &[Speaker], spread: f32, out: &mut [f32]) {
    let spread = if spread.is_nan() {
        0.0
    } else {
        spread.clamp(0.0, 1.0)
    };
    if spread > 0.0 {
        let n = speakers.iter().filter(|s| !s.lfe).count().max(1) as f32;
        for (o, s) in out.iter_mut().zip(speakers) {
            if !s.lfe {
                *o = ((1.0 - spread) * *o * *o + spread / n).sqrt();
            }
        }
    }
}

/// How beds fold into stereo and mono (a project setting).
///
/// With [`DownmixMethod::Coefficients`] a bed folds into stereo as
///
/// ```text
/// Lo = Σ fronts on the left  + centre·C + surround·Σ left surrounds  + height·Σ left tops  + lfe·LFE
/// Ro = Σ fronts on the right + centre·C + surround·Σ right surrounds + height·Σ right tops + lfe·LFE
/// ```
///
/// (a speaker's role and side come from its direction:
/// [`Speaker::role`]); [`Downmix::STANDARD`] is −3 dB (1/√2) for the
/// centre, the surrounds and the tops and leaves the LFE out — the centre
/// and surround coefficients of the common ITU-R BS.775 Lo/Ro downmix (the
/// tops' are FaderFrame's choice; BS.775 has none). With
/// [`DownmixMethod::ByPlace`] every speaker is placed where it stands by
/// the room panner (5.1: `L + 0.707·C + Ls`; what FaderFrame did before
/// this setting existed, and what older projects keep). Into mono a bed
/// becomes `(Lo + Ro) / 2`, as FaderFrame folds any stereo to mono. Into
/// another bed every speaker is placed where it stands, whatever the
/// method; LFE to LFE.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Downmix {
    pub method: DownmixMethod,
    /// The centre into each side (linear gain).
    pub centre: f32,
    /// Each surround into its side.
    pub surround: f32,
    /// Each top into its side.
    pub height: f32,
    /// The LFE into each side (0: left out).
    pub lfe: f32,
}

/// How a [`Downmix`] folds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownmixMethod {
    /// The coefficients.
    Coefficients,
    /// Each speaker placed where it stands (the coefficients unused but
    /// the LFE's).
    ByPlace,
}

impl Default for Downmix {
    fn default() -> Self {
        Self::STANDARD
    }
}

impl Downmix {
    /// −3 dB centre, surrounds and tops; no LFE (new projects).
    pub const STANDARD: Downmix = Downmix {
        method: DownmixMethod::Coefficients,
        centre: std::f32::consts::FRAC_1_SQRT_2,
        surround: std::f32::consts::FRAC_1_SQRT_2,
        height: std::f32::consts::FRAC_1_SQRT_2,
        lfe: 0.0,
    };

    /// Every speaker where it stands (older projects).
    pub const BY_PLACE: Downmix = Downmix {
        method: DownmixMethod::ByPlace,
        centre: std::f32::consts::FRAC_1_SQRT_2,
        surround: 1.0,
        height: 1.0,
        lfe: 0.0,
    };

    /// What projects saved before the setting existed fold with.
    pub fn older() -> Self {
        Self::BY_PLACE
    }

    pub fn is_older(&self) -> bool {
        *self == Self::BY_PLACE
    }

    /// Coefficients in range (0…2, NaN: 0).
    pub fn clamped(self) -> Self {
        let c = |v: f32| if v.is_nan() { 0.0 } else { v.clamp(0.0, 2.0) };
        Self {
            centre: c(self.centre),
            surround: c(self.surround),
            height: c(self.height),
            lfe: c(self.lfe),
            ..self
        }
    }

    /// As numbers for the engine's parameter slots.
    pub fn values(&self) -> [f32; 5] {
        [
            match self.method {
                DownmixMethod::Coefficients => 0.0,
                DownmixMethod::ByPlace => 1.0,
            },
            self.centre,
            self.surround,
            self.height,
            self.lfe,
        ]
    }

    pub fn from_values(v: &[f32; 5]) -> Self {
        Self {
            method: if v[0].round() == 1.0 {
                DownmixMethod::ByPlace
            } else {
                DownmixMethod::Coefficients
            },
            centre: v[1],
            surround: v[2],
            height: v[3],
            lfe: v[4],
        }
        .clamped()
    }

    /// The gains of one speaker of a bed into stereo `(left, right)`.
    fn into_stereo(self, s: &Speaker) -> (f32, f32) {
        if s.lfe {
            return (self.lfe, self.lfe);
        }
        match self.method {
            DownmixMethod::ByPlace => {
                let mut g = [0.0; 2];
                point_gains(&STEREO, s.x, s.y, s.z, 0.0, &mut g);
                (g[0], g[1])
            }
            DownmixMethod::Coefficients => {
                let k = match s.role() {
                    Role::Front => 1.0,
                    Role::Centre => return (self.centre, self.centre),
                    Role::Surround => self.surround,
                    Role::Height => self.height,
                    Role::Lfe => self.lfe,
                };
                match s.side() {
                    Some(Side::Left) => (k, 0.0),
                    Some(Side::Right) => (0.0, k),
                    None => (k, k),
                }
            }
        }
    }
}

/// Bass management of a speaker system (monitoring only: a property of
/// the room, never of the mix or a render).
///
/// With it on, every main speaker is high-passed at `crossover` and the
/// bass taken from them goes to the subwoofer on the layout's LFE channel:
///
/// ```text
/// main'  = HP(main)                                        (each main)
/// sub    = LP_lfe(LFE) + redirect · LP(Σ mains)            (the LFE output)
/// ```
///
/// Both crossover filters are Linkwitz–Riley, 24 dB/octave (two
/// Butterworth sections in a row): a main's high and its redirected low
/// are in phase and add to the main's own level at every frequency
/// (an all-pass) when `redirect` is 0 dB. `redirect_db` is the redirected
/// bass's level on the LFE output; −10 dB (the default) suits a
/// subwoofer calibrated for the LFE's +10 dB in-band gain, so the
/// redirected bass plays at the mains' level. The LFE channel itself only
/// has its band limited (`lfe_lowpass`, also 24 dB/octave) — no gain.
/// Layouts without an LFE channel have no subwoofer output: nothing is
/// managed there.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BassManagement {
    pub enabled: bool,
    /// Hz between the mains and the subwoofer (40…200).
    pub crossover: f32,
    /// The redirected bass on the LFE output, dB (−30…+10).
    pub redirect_db: f32,
    /// The LFE channel's low-pass, Hz (40…250).
    pub lfe_lowpass: f32,
}

impl Default for BassManagement {
    fn default() -> Self {
        Self {
            enabled: false,
            crossover: 80.0,
            redirect_db: -10.0,
            lfe_lowpass: 120.0,
        }
    }
}

impl BassManagement {
    /// In range (a NaN back to its default).
    pub fn clamped(self) -> Self {
        let d = Self::default();
        let c = |v: f32, lo: f32, hi: f32, def: f32| if v.is_nan() { def } else { v.clamp(lo, hi) };
        Self {
            enabled: self.enabled,
            crossover: c(self.crossover, 40.0, 200.0, d.crossover),
            redirect_db: c(self.redirect_db, -30.0, 10.0, d.redirect_db),
            lfe_lowpass: c(self.lfe_lowpass, 40.0, 250.0, d.lfe_lowpass),
        }
    }

    /// As numbers for the engine's parameter slots.
    pub fn values(&self) -> [f32; 4] {
        [
            if self.enabled { 1.0 } else { 0.0 },
            self.crossover,
            self.redirect_db,
            self.lfe_lowpass,
        ]
    }

    pub fn from_values(v: &[f32; 4]) -> Self {
        Self {
            enabled: v[0] >= 0.5,
            crossover: v[1],
            redirect_db: v[2],
            lfe_lowpass: v[3],
        }
        .clamped()
    }

    /// Whether a speaker `layout` can be managed (it has an LFE channel
    /// and a main speaker).
    pub fn applies_to(layout: ChannelLayout) -> bool {
        speakers_of(layout).is_some_and(|s| s.iter().any(|s| s.lfe) && s.iter().any(|s| !s.lfe))
    }
}

/// What, besides the pan, places a source ([`matrix`]).
#[derive(Clone, Copy, Debug)]
pub struct Mixing<'a> {
    /// The destination's VBAP geometry (`None`: VBAP falls back to the
    /// room panner).
    pub vbap: Option<&'a crate::vbap::Vbap>,
    /// How beds fold into stereo and mono.
    pub downmix: Downmix,
    /// The strip's balance (−1…1), for [`StereoSource::Balance`].
    pub balance: f32,
    /// The caller adds the diffuse field ([`diffuse_gain`]): a diffuse
    /// spread leaves the points their share only (else it spreads
    /// coherently).
    pub diffuse: bool,
}

impl Default for Mixing<'_> {
    fn default() -> Self {
        Self {
            vbap: None,
            downmix: Downmix::STANDARD,
            balance: 0.0,
            diffuse: false,
        }
    }
}

/// How a mono, stereo or discrete `src` is placed by `pan` (with VBAP's
/// geometry or not): its stereo mode as it applies.
fn placed_mode(src: ChannelLayout, pan: &SurroundPan, vbap: bool) -> StereoSource {
    match (src == ChannelLayout::Stereo, pan.stereo) {
        (false, _) => StereoSource::Image,
        // Dual needs directions: with the room panner it is an image.
        (true, StereoSource::Dual) if !vbap => StereoSource::Image,
        (true, m) => m,
    }
}

/// The diffuse field's gain on every main speaker of `dst` for a `src`
/// placed by `pan` (0 unless its spread is diffuse): `√(spread / n)` over
/// the `n` main speakers, of the source's channels summed. With the
/// points' share `√(1 − spread)` the power is the coherent spread's —
/// `(1 − s) + n · s/n = 1` — but every speaker gets its own decorrelated
/// copy (the engine's all-pass filters: flat in level, scrambled in
/// phase, no added delay), so off-centre listeners hear no comb filter
/// and a fold to fewer speakers adds the copies in power, not in phase.
pub fn diffuse_gain(src: ChannelLayout, dst: ChannelLayout, pan: &SurroundPan) -> f32 {
    let surround = |l: ChannelLayout| matches!(l, ChannelLayout::Surround(_));
    if surround(src) || !surround(dst) || !pan.diffuse {
        return 0.0;
    }
    if placed_mode(src, pan, pan.panner == SurroundPanner::Vbap) == StereoSource::Balance {
        return 0.0;
    }
    let spread = if pan.spread.is_nan() {
        0.0
    } else {
        pan.spread.clamp(0.0, 1.0)
    };
    let n = speakers_of(dst).map_or(0, |s| s.iter().filter(|s| !s.lfe).count());
    if n == 0 {
        0.0
    } else {
        (spread / n as f32).sqrt()
    }
}

/// Gains from each channel of `src` (rows) to each channel of `dst`
/// (columns): a mono, stereo or discrete source placed by `pan` (a bed
/// keeps its own speakers' places), a bed folded by `mixing.downmix`.
/// Returns whether the matrix applies (`false`: neither side is a bed, the
/// layouts pass channel by channel as before, e.g. stereo to stereo).
pub fn matrix(
    src: ChannelLayout,
    dst: ChannelLayout,
    pan: &SurroundPan,
    mixing: &Mixing<'_>,
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
        // A bed into stereo or mono: the downmix.
        Some(from) if surround(src) && !surround(dst) => {
            let mono = dst.channel_count() == 1;
            for (row, s) in out.iter_mut().zip(from) {
                let (l, r) = mixing.downmix.into_stereo(s);
                if mono {
                    row[0] = 0.5 * (l + r);
                } else {
                    row[0] = l;
                    row[1] = r;
                }
            }
        }
        // A bed into another: each speaker where it stands; LFE to LFE.
        // Into or out of a custom layout by its direction (VBAP over the
        // destination), between built-in formats by its room place.
        Some(from) if surround(src) => {
            let custom =
                |l: ChannelLayout| matches!(l, ChannelLayout::Surround(f) if f.custom().is_some());
            let vbap = mixing.vbap.filter(|_| custom(src) || custom(dst));
            for (row, s) in out.iter_mut().zip(from) {
                if s.lfe {
                    if let Some(l) = lfe_out {
                        row[l] = 1.0;
                    }
                } else if let Some(v) = vbap {
                    v.gains(s.azimuth, s.elevation, &mut row[..to.len()]);
                } else {
                    point_gains(to, s.x, s.y, s.z, 0.0, row);
                }
            }
        }
        // Mono, stereo or discrete into a bed: placed by the panner.
        _ => place(src, to, &pan, mixing, out),
    }
    true
}

/// A mono, stereo or discrete source placed into the bed `to`.
fn place(
    src: ChannelLayout,
    to: &[Speaker],
    pan: &SurroundPan,
    mixing: &Mixing<'_>,
    out: &mut [[f32; MAX_SPEAKERS]; MAX_SPEAKERS],
) {
    let n = src.channel_count().min(MAX_SPEAKERS);
    let lfe_out = to.iter().position(|s| s.lfe);
    let vbap = mixing.vbap.filter(|_| pan.panner == SurroundPanner::Vbap);
    let stereo = src == ChannelLayout::Stereo;
    let mode = placed_mode(src, pan, vbap.is_some());
    let front = |label: &str| to.iter().position(|s| s.label == label);
    if let (StereoSource::Balance, Some(l), Some(r)) = (mode, front("L"), front("R")) {
        let (gl, gr) = crate::pan::stereo_balance(mixing.balance);
        out[0][l] = gl;
        out[1][r] = gr;
    } else {
        for (c, row) in out.iter_mut().enumerate().take(n) {
            let side = if c == 0 { 1.0 } else { -1.0 };
            match vbap {
                Some(v) => {
                    let azimuth = match mode {
                        StereoSource::Dual if c == 0 => pan.left,
                        StereoSource::Dual => pan.right,
                        _ if stereo => pan.azimuth + side * 30.0 * pan.width,
                        _ => pan.azimuth,
                    };
                    v.gains(azimuth, pan.elevation, &mut row[..to.len()]);
                }
                None => {
                    let x = if stereo {
                        pan.x - side * pan.width
                    } else {
                        pan.x
                    };
                    point_gains(to, x.clamp(-1.0, 1.0), pan.y, pan.z, 0.0, row);
                }
            }
            if pan.diffuse && mixing.diffuse {
                // The points keep their share; the rest is the diffuse
                // field the caller adds.
                let keep = (1.0 - pan.spread.clamp(0.0, 1.0)).sqrt();
                for g in row.iter_mut().take(to.len()) {
                    *g *= keep;
                }
            } else {
                spread_over(to, pan.spread, row);
            }
        }
    }
    // The LFE: its own send, shared by the source's channels.
    if let Some(l) = lfe_out {
        for row in out.iter_mut().take(n) {
            row[l] = pan.lfe_gain() / n as f32;
        }
    }
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
        let by_place = Mixing {
            downmix: Downmix::BY_PLACE,
            ..Mixing::default()
        };
        let matrix = |src, dst, pan: &SurroundPan, m: &mut _| matrix(src, dst, pan, &by_place, m);
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

#[cfg(test)]
mod placing {
    use super::*;
    use crate::vbap::Vbap;
    use std::f32::consts::FRAC_1_SQRT_2 as H;

    type M = [[f32; MAX_SPEAKERS]; MAX_SPEAKERS];

    fn layout(f: SurroundFormat) -> ChannelLayout {
        ChannelLayout::Surround(f)
    }

    fn at(f: SurroundFormat, label: &str) -> usize {
        f.speakers().iter().position(|s| s.label == label).unwrap()
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    fn fold(f: SurroundFormat, to: ChannelLayout, downmix: Downmix) -> M {
        let mut m = [[0.0; MAX_SPEAKERS]; MAX_SPEAKERS];
        let mixing = Mixing {
            downmix,
            ..Mixing::default()
        };
        assert!(matrix(
            layout(f),
            to,
            &SurroundPan::default(),
            &mixing,
            &mut m
        ));
        m
    }

    #[test]
    fn five_one_folds_to_stereo_by_its_coefficients() {
        let f = SurroundFormat::S51;
        let m = fold(f, ChannelLayout::Stereo, Downmix::STANDARD);
        let row = |l: &str| (m[at(f, l)][0], m[at(f, l)][1]);
        assert_eq!(row("L"), (1.0, 0.0));
        assert_eq!(row("R"), (0.0, 1.0));
        assert_eq!(row("C"), (H, H));
        assert_eq!(row("Ls"), (H, 0.0));
        assert_eq!(row("Rs"), (0.0, H));
        // The LFE is left out unless asked for.
        assert_eq!(row("LFE"), (0.0, 0.0));
        let with_lfe = Downmix {
            lfe: 0.5,
            ..Downmix::STANDARD
        };
        let m = fold(f, ChannelLayout::Stereo, with_lfe);
        assert_eq!((m[3][0], m[3][1]), (0.5, 0.5));
        // Nothing beyond the six channels or the two outputs.
        assert!(m.iter().flat_map(|r| &r[2..]).all(|v| *v == 0.0));
        assert!(m[6..].iter().flatten().all(|v| *v == 0.0));
    }

    #[test]
    fn seven_one_and_its_heights_fold_by_role() {
        let f = SurroundFormat::S714;
        let custom = Downmix {
            centre: 0.5,
            surround: 0.6,
            height: 0.3,
            ..Downmix::STANDARD
        };
        let m = fold(f, ChannelLayout::Stereo, custom);
        for (label, l, r) in [
            ("L", 1.0, 0.0),
            ("C", 0.5, 0.5),
            ("Lss", 0.6, 0.0),
            ("Lrs", 0.6, 0.0),
            ("Rss", 0.0, 0.6),
            ("Rrs", 0.0, 0.6),
            ("Ltf", 0.3, 0.0),
            ("Rtr", 0.0, 0.3),
            ("LFE", 0.0, 0.0),
        ] {
            let i = at(f, label);
            assert_eq!((m[i][0], m[i][1]), (l, r), "{label}");
        }
        let f = SurroundFormat::S71;
        let m = fold(f, ChannelLayout::Stereo, Downmix::STANDARD);
        assert_eq!((m[at(f, "Lss")][0], m[at(f, "Lrs")][0]), (H, H));
    }

    #[test]
    fn mono_is_the_stereo_fold_halved() {
        let f = SurroundFormat::S51;
        for downmix in [Downmix::STANDARD, Downmix::BY_PLACE] {
            let stereo = fold(f, ChannelLayout::Stereo, downmix);
            let mono = fold(f, ChannelLayout::Mono, downmix);
            for c in 0..f.channels() {
                assert!(close(mono[c][0], 0.5 * (stereo[c][0] + stereo[c][1])));
                assert!(mono[c][1..].iter().all(|v| *v == 0.0));
            }
        }
        let m = fold(f, ChannelLayout::Mono, Downmix::STANDARD);
        assert_eq!(m[at(f, "L")][0], 0.5);
        assert_eq!(m[at(f, "C")][0], H);
        assert!(close(m[at(f, "Ls")][0], 0.5 * H));
        // The fold of older projects no longer sums every speaker at
        // unity into mono (it reached +14 dB).
        let m = fold(f, ChannelLayout::Mono, Downmix::BY_PLACE);
        let sum: f32 = (0..6).map(|c| m[c][0]).sum();
        assert!(sum < 3.0, "{sum}");
    }

    #[test]
    fn a_full_scale_bed_folds_to_a_known_peak() {
        // No normalisation: a bed whose every channel plays the same full
        // scale signal peaks at the sum of its coefficients — headroom the
        // mix must leave (1 + √2 = +7.66 dB for 5.1 → stereo).
        let f = SurroundFormat::S51;
        let m = fold(f, ChannelLayout::Stereo, Downmix::STANDARD);
        let peak: f32 = (0..6).map(|c| m[c][0]).sum();
        assert!(close(peak, 1.0 + std::f32::consts::SQRT_2), "{peak}");
        // Uncorrelated channels add in power: 1 + ½ + ½ = 2 (+3 dB).
        let power: f32 = (0..6).map(|c| m[c][0] * m[c][0]).sum();
        assert!(close(power, 2.0), "{power}");
    }

    fn place(f: SurroundFormat, src: ChannelLayout, pan: SurroundPan, balance: f32) -> M {
        let vbap = Vbap::of(f);
        let mixing = Mixing {
            vbap: Some(&vbap),
            downmix: Downmix::STANDARD,
            balance,
            diffuse: true,
        };
        let mut m = [[0.0; MAX_SPEAKERS]; MAX_SPEAKERS];
        assert!(matrix(src, layout(f), &pan, &mixing, &mut m));
        m
    }

    #[test]
    fn a_diffuse_spread_keeps_the_power_and_leaves_the_points_their_share() {
        let f = SurroundFormat::S51;
        let pan = SurroundPan {
            spread: 0.6,
            diffuse: true,
            ..vbap()
        };
        let m = place(f, ChannelLayout::Mono, pan, 0.0);
        // The point: C alone, at √(1 − 0.6).
        assert!(close(m[0][at(f, "C")], 0.4f32.sqrt()));
        assert_eq!(m[0][at(f, "L")], 0.0);
        // The diffuse field: √(0.6 / 5) on each of the five mains.
        let d = diffuse_gain(ChannelLayout::Mono, layout(f), &pan);
        assert!(close(d, (0.6f32 / 5.0).sqrt()));
        let power = m[0].iter().map(|g| g * g).sum::<f32>() + 5.0 * d * d;
        assert!(close(power, 1.0), "{power}");
        // Coherent (or without the field): the same signal on all.
        let coherent = place(
            f,
            ChannelLayout::Mono,
            SurroundPan {
                diffuse: false,
                ..pan
            },
            0.0,
        );
        assert!(coherent[0][at(f, "L")] > 0.0);
        assert_eq!(
            diffuse_gain(
                ChannelLayout::Mono,
                layout(f),
                &SurroundPan {
                    diffuse: false,
                    ..pan
                }
            ),
            0.0
        );
        // Balanced stereo spreads nothing; beds and stereo outputs neither.
        let bal = SurroundPan {
            stereo: StereoSource::Balance,
            ..pan
        };
        assert_eq!(diffuse_gain(ChannelLayout::Stereo, layout(f), &bal), 0.0);
        assert_eq!(diffuse_gain(layout(f), ChannelLayout::Stereo, &pan), 0.0);
        assert_eq!(
            diffuse_gain(layout(f), layout(SurroundFormat::S71), &pan),
            0.0
        );
        // It travels with the other values.
        assert!(SurroundPan::from_values(&pan.values()).diffuse);
    }

    fn vbap() -> SurroundPan {
        SurroundPan {
            panner: SurroundPanner::Vbap,
            ..SurroundPan::default()
        }
    }

    #[test]
    fn a_mono_source_pans_by_vbap_and_never_into_the_lfe() {
        for f in [SurroundFormat::S51, SurroundFormat::S71] {
            for az in [-180.0f32, -110.0, -30.0, 0.0, 15.0, 90.0, 135.0] {
                let m = place(
                    f,
                    ChannelLayout::Mono,
                    SurroundPan {
                        azimuth: az,
                        ..vbap()
                    },
                    0.0,
                );
                let mut want = [0.0; MAX_SPEAKERS];
                Vbap::of(f).gains(az, 0.0, &mut want[..f.channels()]);
                assert_eq!(m[0], want, "{} {az}", f.name());
                assert_eq!(m[0][at(f, "LFE")], 0.0);
            }
        }
        // The LFE only by its send, wherever the source is.
        let f = SurroundFormat::S51;
        let m = place(
            f,
            ChannelLayout::Mono,
            SurroundPan {
                azimuth: 180.0,
                lfe_db: -6.0206,
                ..vbap()
            },
            0.0,
        );
        assert!(close(m[0][at(f, "LFE")], 0.5));
        let power: f32 = m[0]
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != 3)
            .map(|(_, g)| g * g)
            .sum();
        assert!(
            close(power, 1.0),
            "the send adds, the mains keep their power"
        );
    }

    #[test]
    fn a_stereo_image_keeps_its_sides_where_the_format_can() {
        let f = SurroundFormat::S51;
        // At its default the image is the format's L and R: nothing else.
        let m = place(f, ChannelLayout::Stereo, vbap(), 0.0);
        assert!(close(m[0][at(f, "L")], 1.0) && close(m[1][at(f, "R")], 1.0));
        assert_eq!(m[0][at(f, "R")] + m[1][at(f, "L")] + m[0][at(f, "C")], 0.0);
        // Narrowed to nothing: both channels at the centre (their sum plays
        // there; opposite-phase material cancels, as in any mono fold).
        let m = place(
            f,
            ChannelLayout::Stereo,
            SurroundPan {
                width: 0.0,
                ..vbap()
            },
            0.0,
        );
        assert_eq!(m[0], m[1]);
        assert!(close(m[0][at(f, "C")], 1.0));
        // Turned to the back: rotated as a whole (its left now on the
        // listener's right-back).
        let m = place(
            f,
            ChannelLayout::Stereo,
            SurroundPan {
                azimuth: 180.0,
                ..vbap()
            },
            0.0,
        );
        assert!(m[0][at(f, "Rs")] > m[0][at(f, "Ls")], "{:?}", m[0]);
        assert!(m[1][at(f, "Ls")] > m[1][at(f, "Rs")], "{:?}", m[1]);
    }

    #[test]
    fn balance_and_dual_place_stereo_their_own_ways() {
        let f = SurroundFormat::S71;
        let bal = SurroundPan {
            stereo: StereoSource::Balance,
            spread: 0.8,
            ..vbap()
        };
        // Balance: L to L, R to R, at the strip's balance; unity at
        // centre; no spread, no crossfeed.
        let m = place(f, ChannelLayout::Stereo, bal, 0.0);
        let mut want = [[0.0; MAX_SPEAKERS]; 2];
        want[0][at(f, "L")] = 1.0;
        want[1][at(f, "R")] = 1.0;
        assert_eq!([m[0], m[1]], want);
        let m = place(f, ChannelLayout::Stereo, bal, 0.5);
        assert_eq!((m[0][at(f, "L")], m[1][at(f, "R")]), (0.5, 1.0));
        // The room panner balances the same way.
        let m = place(
            f,
            ChannelLayout::Stereo,
            SurroundPan {
                panner: SurroundPanner::Room,
                ..bal
            },
            -1.0,
        );
        assert_eq!((m[0][at(f, "L")], m[1][at(f, "R")]), (1.0, 0.0));
        // Dual: each channel exactly where it is put.
        let dual = SurroundPan {
            stereo: StereoSource::Dual,
            left: 90.0,
            right: -135.0,
            ..vbap()
        };
        let m = place(f, ChannelLayout::Stereo, dual, 0.0);
        assert!(close(m[0][at(f, "Lss")], 1.0) && close(m[1][at(f, "Rrs")], 1.0));
        assert!(close(m[0].iter().map(|g| g * g).sum::<f32>(), 1.0));
        // Both on one direction: they add there (no swapping, no
        // cancelling of their own).
        let m = place(
            f,
            ChannelLayout::Stereo,
            SurroundPan {
                left: -90.0,
                right: -90.0,
                ..dual
            },
            0.0,
        );
        assert_eq!(m[0], m[1]);
        assert!(close(m[0][at(f, "Rss")], 1.0));
        // With the room panner dual pan has no directions: an image.
        let room_dual = place(
            f,
            ChannelLayout::Stereo,
            SurroundPan {
                panner: SurroundPanner::Room,
                ..dual
            },
            0.0,
        );
        let room_image = place(
            f,
            ChannelLayout::Stereo,
            SurroundPan {
                panner: SurroundPanner::Room,
                ..SurroundPan::default()
            },
            0.0,
        );
        assert_eq!(room_dual, room_image);
    }

    #[test]
    fn values_round_trip_and_what_applies() {
        let pan = SurroundPan {
            x: -0.3,
            azimuth: 200.0, // wraps to −160
            elevation: 120.0,
            panner: SurroundPanner::Vbap,
            stereo: StereoSource::Dual,
            left: 45.0,
            right: -400.0,
            ..SurroundPan::default()
        };
        let back = SurroundPan::from_values(&pan.values());
        assert_eq!(back, pan.clamped());
        assert!(close(back.azimuth, -160.0) && back.elevation == 90.0);
        assert!(close(back.right, -40.0));
        assert_eq!(
            SurroundParam::Azimuth
                .set(SurroundPan::default(), 180.0)
                .azimuth,
            180.0
        );
        assert_eq!(
            SurroundParam::Azimuth
                .set(SurroundPan::default(), f32::NAN)
                .azimuth,
            0.0
        );
        let applies = |pan: &SurroundPan, src, f: SurroundFormat| -> Vec<SurroundParam> {
            SurroundParam::ALL
                .into_iter()
                .filter(|p| p.applies(src, f, pan))
                .collect()
        };
        use SurroundParam as P;
        let (mono, stereo) = (ChannelLayout::Mono, ChannelLayout::Stereo);
        let room = SurroundPan::default();
        assert_eq!(
            applies(&room, mono, SurroundFormat::S51),
            [P::X, P::Y, P::Spread, P::Lfe]
        );
        assert_eq!(
            applies(&vbap(), mono, SurroundFormat::S714),
            [P::Spread, P::Lfe, P::Azimuth, P::Elevation]
        );
        assert_eq!(
            applies(&vbap(), stereo, SurroundFormat::S50),
            [P::Spread, P::Width, P::Azimuth]
        );
        let dual = SurroundPan {
            stereo: StereoSource::Dual,
            ..vbap()
        };
        assert_eq!(
            applies(&dual, stereo, SurroundFormat::S51),
            [P::Spread, P::Lfe, P::LeftAzimuth, P::RightAzimuth]
        );
        let balance = SurroundPan {
            stereo: StereoSource::Balance,
            ..vbap()
        };
        assert_eq!(applies(&balance, stereo, SurroundFormat::S51), [P::Lfe]);
        // A mono source has no stereo mode, whatever is stored.
        assert_eq!(
            applies(&balance, mono, SurroundFormat::S50),
            [P::Spread, P::Azimuth]
        );
    }

    #[test]
    fn older_pans_read_as_they_always_did() {
        // A pan saved before VBAP: only the room's fields.
        let old: SurroundPan = serde_json::from_str(r#"{"x":-0.5,"y":-1.0,"width":0.4}"#).unwrap();
        assert_eq!(old.panner, SurroundPanner::Room);
        assert_eq!(old.stereo, StereoSource::Image);
        assert_eq!((old.left, old.right), (30.0, -30.0));
        // Only what differs from the defaults is written.
        assert_eq!(
            serde_json::to_string(&SurroundPan::default()).unwrap(),
            "{}"
        );
        let v = serde_json::to_string(&SurroundPan {
            azimuth: 45.0,
            ..vbap()
        })
        .unwrap();
        assert_eq!(v, r#"{"panner":"vbap","azimuth":45.0}"#);
    }

    #[test]
    fn every_speaker_has_a_role_and_a_side() {
        let f = SurroundFormat::S714;
        let role = |l: &str| f.speakers()[at(f, l)].role();
        assert_eq!(role("L"), Role::Front);
        assert_eq!(role("C"), Role::Centre);
        assert_eq!(role("Lss"), Role::Surround);
        assert_eq!(role("Rrs"), Role::Surround);
        assert_eq!(role("Ltf"), Role::Height);
        assert_eq!(role("LFE"), Role::Lfe);
        for s in f.speakers() {
            let side = s.side();
            match s.label {
                "C" | "LFE" => assert_eq!(side, None),
                l if l.starts_with('L') => assert_eq!(side, Some(Side::Left), "{l}"),
                l => assert_eq!(side, Some(Side::Right), "{l}"),
            }
        }
        // Places and directions agree on the side of every speaker.
        for f in SurroundFormat::ALL {
            for s in f.speakers().iter().filter(|s| !s.lfe) {
                let x = s.direction()[0];
                assert!(
                    x.signum() == s.x.signum() || s.x == 0.0,
                    "{} {}",
                    f.name(),
                    s.label
                );
                assert_eq!(s.elevation > 0.0, s.z > 0.0);
            }
        }
    }
}

#[cfg(test)]
mod custom {
    use super::*;
    use crate::vbap::Vbap;

    fn spk(label: &str, azimuth: f32, elevation: f32) -> SpeakerDef {
        SpeakerDef {
            label: label.into(),
            azimuth,
            elevation,
            lfe: false,
        }
    }

    fn hexagon() -> LayoutDef {
        LayoutDef {
            name: "Test Hexagon".into(),
            speakers: vec![
                spk("F", 0.0, 0.0),
                spk("FL", 60.0, 0.0),
                spk("BL", 120.0, 0.0),
                spk("B", 180.0, 0.0),
                spk("BR", -120.0, 0.0),
                spk("FR", -60.0, 0.0),
            ],
        }
    }

    #[test]
    fn a_definition_is_one_format_and_reads_back() {
        let a = hexagon().intern().unwrap();
        let b = hexagon().intern().unwrap();
        assert_eq!(a, b, "the same definition, the same format");
        assert!(std::ptr::eq(a.custom().unwrap(), b.custom().unwrap()));
        let mut other = hexagon();
        other.speakers[3].azimuth = 175.0;
        assert_ne!(other.intern().unwrap(), a);
        assert_ne!(a, SurroundFormat::S51);
        assert_eq!((a.name(), a.channels()), ("Test Hexagon", 6));
        assert!(!a.has_lfe() && !a.has_heights());
        // In a project file it is written out, and reads back as itself.
        let layout = ChannelLayout::Surround(a);
        let json = serde_json::to_string(&layout).unwrap();
        assert!(
            json.contains("\"FL\"") && json.contains("Test Hexagon"),
            "{json}"
        );
        let back: ChannelLayout = serde_json::from_str(&json).unwrap();
        assert_eq!(back, layout);
        // Built-in formats still read and write by name.
        let s51 = ChannelLayout::Surround(SurroundFormat::S51);
        assert_eq!(
            serde_json::to_string(&s51).unwrap(),
            r#"{"surround":"5.1"}"#
        );
        let quad: ChannelLayout = serde_json::from_str(r#"{"surround":"quad"}"#).unwrap();
        assert_eq!(quad, ChannelLayout::Surround(SurroundFormat::Quad));
        // A broken one in a file is refused, not misread.
        assert!(serde_json::from_str::<ChannelLayout>(r#"{"surround":"9.1"}"#).is_err());
        assert!(
            serde_json::from_str::<ChannelLayout>(r#"{"surround":{"name":"X","speakers":[]}}"#)
                .is_err()
        );
    }

    #[test]
    fn bad_layouts_are_refused() {
        let with = |f: &dyn Fn(&mut LayoutDef)| {
            let mut d = hexagon();
            f(&mut d);
            d.validate().err()
        };
        assert_eq!(with(&|d| d.name = " ".into()), Some(LayoutError::NoName));
        assert!(matches!(
            with(&|d| d.name = "5.1".into()),
            Some(LayoutError::TakenName(_))
        ));
        assert_eq!(with(&|d| d.speakers.clear()), Some(LayoutError::NoSpeakers));
        assert_eq!(
            with(&|d| d.speakers[2].label = "fl".into()),
            Some(LayoutError::SameLabel(2))
        );
        assert_eq!(
            with(&|d| d.speakers[1].label.clear()),
            Some(LayoutError::BadLabel(1))
        );
        assert_eq!(
            with(&|d| d.speakers[4].elevation = 95.0),
            Some(LayoutError::BadDirection(4))
        );
        assert!(matches!(
            with(&|d| d.speakers[2].azimuth = 60.5),
            Some(LayoutError::Geometry(
                crate::vbap::VbapError::SameDirection(1, 2)
            ))
        ));
        let lfe = SpeakerDef {
            lfe: true,
            ..spk("LFE", 0.0, 0.0)
        };
        assert_eq!(
            with(&|d| {
                d.speakers.push(lfe.clone());
                d.speakers.push(SpeakerDef {
                    label: "LFE2".into(),
                    ..lfe.clone()
                });
            }),
            Some(LayoutError::TwoLfe)
        );
        assert_eq!(
            with(&|d| d.speakers = vec![lfe.clone()]),
            Some(LayoutError::NoMain)
        );
        assert_eq!(
            with(&|d| {
                d.speakers = (0..=MAX_SPEAKERS)
                    .map(|i| spk(&format!("S{i}"), i as f32 * 20.0 - 170.0, 0.0))
                    .collect()
            }),
            Some(LayoutError::TooMany)
        );
        // All to the front and one up: the listener is outside.
        assert!(matches!(
            with(&|d| {
                d.speakers = vec![
                    spk("A", 10.0, 0.0),
                    spk("B", -10.0, 0.0),
                    spk("C", 0.0, 20.0),
                ]
            }),
            Some(LayoutError::Geometry(crate::vbap::VbapError::NotAround))
        ));
    }

    #[test]
    fn its_speakers_stand_where_their_directions_say() {
        let f = hexagon().intern().unwrap();
        let s = f.speakers();
        // Room places on the walls, by direction.
        assert!(s[1].x < -0.5 && s[1].y > 0.0, "FL: {:?}", s[1]);
        assert!((s[3].y + 1.0).abs() < 1e-6 && s[3].x.abs() < 1e-6, "B");
        assert_eq!(s[1].side(), Some(Side::Left));
        assert_eq!(s[0].role(), Role::Centre);
        assert_eq!(s[1].role(), Role::Surround);
        // VBAP over it: 60° left is FL alone; 30° shared with F.
        let v = Vbap::of(f);
        let mut g = [0.0; 6];
        v.gains(60.0, 0.0, &mut g);
        assert!((g[1] - 1.0).abs() < 1e-5);
        v.gains(30.0, 0.0, &mut g);
        assert!((g[0] - g[1]).abs() < 1e-5 && g[0] > 0.7);
    }

    #[test]
    fn custom_layouts_fold_by_direction() {
        let hex = hexagon().intern().unwrap();
        let s51 = SurroundFormat::S51;
        let mut m = [[0.0; MAX_SPEAKERS]; MAX_SPEAKERS];
        // Into 5.1: each speaker by VBAP at its direction.
        let v = Vbap::of(s51);
        let mixing = Mixing {
            vbap: Some(&v),
            ..Mixing::default()
        };
        let pan = SurroundPan::default();
        assert!(matrix(
            ChannelLayout::Surround(hex),
            ChannelLayout::Surround(s51),
            &pan,
            &mixing,
            &mut m
        ));
        assert!((m[0][2] - 1.0).abs() < 1e-5, "F on C: {:?}", &m[0][..6]);
        assert!(
            (m[3][4] - m[3][5]).abs() < 1e-5 && m[3][4] > 0.7,
            "B between Ls/Rs"
        );
        // FL at 60°: between L (30°) and Ls (110°).
        assert!(m[1][0] > 0.0 && m[1][4] > 0.0 && m[1][2] == 0.0);
        // And from 5.1 into it.
        let vh = Vbap::of(hex);
        let mixing = Mixing {
            vbap: Some(&vh),
            ..Mixing::default()
        };
        assert!(matrix(
            ChannelLayout::Surround(s51),
            ChannelLayout::Surround(hex),
            &pan,
            &mixing,
            &mut m
        ));
        assert!((m[2][0] - 1.0).abs() < 1e-5, "C on F");
        assert_eq!(m[3], [0.0; MAX_SPEAKERS], "no LFE to go to");
        // Into stereo by the downmix's roles.
        assert!(matrix(
            ChannelLayout::Surround(hex),
            ChannelLayout::Stereo,
            &pan,
            &Mixing::default(),
            &mut m
        ));
        let h = std::f32::consts::FRAC_1_SQRT_2;
        assert_eq!((m[0][0], m[0][1]), (h, h), "F is a centre");
        assert_eq!((m[1][0], m[1][1]), (h, 0.0), "FL a left surround");
    }

    #[test]
    fn speakers_below_ear_level_take_sources_below_it() {
        // A ring, four up and four down: a sphere around the listener.
        let mut speakers: Vec<SpeakerDef> = (0..6)
            .map(|i| spk(&format!("M{i}"), 180.0 - 60.0 * i as f32 - 30.0, 0.0))
            .collect();
        for (i, el) in [(0, 40.0f32), (1, -40.0)] {
            speakers.extend(
                [45.0f32, 135.0, -135.0, -45.0]
                    .iter()
                    .map(|az| spk(&format!("{}{az}", if i == 0 { "U" } else { "D" }), *az, el)),
            );
        }
        let f = LayoutDef {
            name: "Sphere".into(),
            speakers,
        }
        .intern()
        .unwrap();
        assert!(f.has_lower() && f.is_3d());
        assert_eq!(f.elevation_range(), (-90.0, 90.0));
        assert_eq!(SurroundFormat::S714.elevation_range(), (0.0, 90.0));
        assert_eq!(SurroundParam::Elevation.range_in(f).0, -90.0);
        let v = Vbap::of(f);
        assert_eq!(v.min_elevation(), -90.0);
        let mut g = [0.0; 14];
        // At a lower speaker: it alone.
        v.gains(45.0, -40.0, &mut g);
        assert!((g[10] - 1.0).abs() < 1e-5, "{g:?}");
        // Straight down: the four lower ones alike; nothing up.
        v.gains(0.0, -90.0, &mut g);
        assert!(g[10..14].iter().all(|x| (x - 0.5).abs() < 1e-4), "{g:?}");
        assert!(g[6..10].iter().all(|x| *x == 0.0));
        // Up and down mirror each other, every step smooth.
        let mut up = [0.0; 14];
        let mut last: Option<[f32; 14]> = None;
        for e in -90..=90 {
            v.gains(100.0, e as f32, &mut g);
            assert!((g.iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-4);
            v.gains(100.0, -(e as f32), &mut up);
            for k in 0..6 {
                assert!((g[k] - up[k]).abs() < 1e-4, "ring at {e}");
            }
            for k in 0..4 {
                assert!((g[6 + k] - up[10 + k]).abs() < 1e-4, "tops/bottoms at {e}");
            }
            if let Some(prev) = last {
                let jump = prev
                    .iter()
                    .zip(&g)
                    .map(|(a, b)| (a - b).abs())
                    .fold(0.0f32, f32::max);
                assert!(jump < 0.08, "{e}: {jump}");
            }
            last = Some(g);
        }
        // A built-in format still stops at ear level.
        let mut g51 = [0.0; 6];
        let mut level = [0.0; 6];
        Vbap::of(SurroundFormat::S51).gains(30.0, -40.0, &mut g51);
        Vbap::of(SurroundFormat::S51).gains(30.0, 0.0, &mut level);
        assert_eq!(g51, level);
    }

    #[test]
    fn sixteen_channels_in_three_dimensions() {
        let mut speakers: Vec<SpeakerDef> = (0..8)
            .map(|i| spk(&format!("M{i}"), i as f32 * 45.0 - 135.0, 0.0))
            .collect();
        speakers.extend((0..6).map(|i| spk(&format!("U{i}"), i as f32 * 60.0 - 150.0, 35.0)));
        speakers.push(spk("Top", 0.0, 90.0));
        speakers.push(SpeakerDef {
            lfe: true,
            ..spk("LFE", 0.0, 0.0)
        });
        let f = LayoutDef {
            name: "Dome 16".into(),
            speakers,
        }
        .intern()
        .unwrap();
        assert_eq!(f.channels(), 16);
        let v = Vbap::of(f);
        assert!(v.is_3d());
        let mut g = [0.0; 16];
        v.gains(0.0, 90.0, &mut g);
        assert!((g[14] - 1.0).abs() < 1e-5, "overhead on Top");
        for (az, el) in [(10.0f32, 20.0f32), (-170.0, 60.0), (90.0, 0.0)] {
            v.gains(az, el, &mut g);
            assert!((g.iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-4);
            assert_eq!(g[15], 0.0, "never the LFE");
        }
    }
}
