//! 360° metadata in movie files: read when a file is probed, written into
//! the movies made from it.
//!
//! - MP4 and QuickTime: Google's Spherical Video V2 (`st3d` and `sv3d` in
//!   the picture's sample entry: projection, pose, stereo layout) and V1
//!   (a `uuid` box of XML in its track; equirectangular only), and spatial
//!   audio (`SA3D` in a sound's sample entry: an ambisonic field, here
//!   always ACN order and SN3D normalisation — AmbiX).
//! - Matroska and WebM: V2's `Projection` in the track's `Video` element
//!   and its `StereoMode`, and V1's `spherical-video` tag. Read only.
//!
//! GStreamer neither reads nor writes these, so the boxes are handled
//! here: reading parses the `moov` box (or Matroska's `Tracks` and
//! `Tags`); writing rewrites `moov` with the boxes added and moves the
//! chunk offsets (`stco`, `co64`) of the media stored after it by the
//! bytes added — the media itself is copied untouched. Specifications:
//! github.com/google/spatial-media `docs/spherical-video-v2-rfc.md`,
//! `spherical-video-rfc.md` and `spatial-audio-rfc.md`.

use crate::{Result, VideoError};
use faderframe_core::AmbiOrder;
use faderframe_core::spherical::{Crop, Pose, Projection, Spherical, StereoMode};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// The V1 box's UUID (`ffcc8263-f855-4a93-8814-587a02521fdd`).
pub const V1_UUID: [u8; 16] = [
    0xff, 0xcc, 0x82, 0x63, 0xf8, 0x55, 0x4a, 0x93, 0x88, 0x14, 0x58, 0x7a, 0x02, 0x52, 0x1f, 0xdd,
];

/// What the metadata names as its maker.
const TOOL: &str = "FaderFrame";

/// `moov` boxes larger than this are not read (a broken file).
const MAX_MOOV: u64 = 256 << 20;
/// Matroska elements read into memory at most.
const MAX_ELEMENT: u64 = 16 << 20;

/// An ambisonic sound track's layout (`SA3D`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpatialAudio {
    /// The ambisonic order.
    pub order: u32,
    /// The ambisonic component (ACN) each channel holds, in the file's
    /// channel order; with head-locked stereo the two after the
    /// components are its left and right.
    pub map: Vec<u32>,
    /// Two channels of stereo that stay with the head come with the field.
    #[serde(default)]
    pub head_locked_stereo: bool,
    /// Periphonic, ACN, SN3D (the only kind the RFC defines; anything
    /// else is kept but not taken as a field).
    #[serde(default = "yes")]
    pub ambix: bool,
}

fn yes() -> bool {
    true
}

impl SpatialAudio {
    /// A field of `order` in AmbiX as it is (channel `k` = ACN `k`).
    pub fn ambix(order: AmbiOrder) -> Self {
        Self {
            order: order.order() as u32,
            map: (0..order.channels() as u32).collect(),
            head_locked_stereo: false,
            ambix: true,
        }
    }

    /// The AmbiX field the track is as stored: every channel a component
    /// in ACN order, nothing else.
    pub fn field(&self) -> Option<AmbiOrder> {
        let order = AmbiOrder::from_order(self.order as usize)?;
        let plain = self.ambix
            && !self.head_locked_stereo
            && self.map.len() == order.channels()
            && self.map.iter().enumerate().all(|(i, &c)| c == i as u32);
        plain.then_some(order)
    }

    /// The `SA3D` box.
    pub fn to_box(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(12 + 4 * self.map.len());
        b.push(0); // version
        b.push(if self.head_locked_stereo { 0x80 } else { 0 }); // periphonic
        b.extend_from_slice(&self.order.to_be_bytes());
        b.push(0); // ACN
        b.push(0); // SN3D
        b.extend_from_slice(&(self.map.len() as u32).to_be_bytes());
        for c in &self.map {
            b.extend_from_slice(&c.to_be_bytes());
        }
        boxed(*b"SA3D", &b)
    }

    fn from_body(b: &[u8]) -> Option<Self> {
        if b.len() < 12 || b[0] != 0 {
            return None;
        }
        let head_locked_stereo = b[1] & 0x80 != 0;
        let kind = b[1] & 0x7f;
        let order = be32(&b[2..])?;
        let (ordering, normalisation) = (b[6], b[7]);
        let n = be32(&b[8..])? as usize;
        let map = (0..n)
            .map(|i| be32(b.get(12 + 4 * i..)?))
            .collect::<Option<Vec<u32>>>()?;
        Some(Self {
            order,
            map,
            head_locked_stereo,
            ambix: kind == 0 && ordering == 0 && normalisation == 0,
        })
    }
}

/// What a file says about itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Found {
    /// The (first) picture's 360° layout.
    pub video: Option<Spherical>,
    /// Each sound track's spatial audio, in the file's order.
    pub audio: Vec<Option<SpatialAudio>>,
}

/// Read a file's 360° metadata (MP4/QuickTime or Matroska/WebM; anything
/// else has none).
pub fn read(path: &Path) -> Result<Found> {
    let mut f = BufReader::new(File::open(path)?);
    let mut magic = [0u8; 8];
    let n = read_up_to(&mut f, &mut magic)?;
    f.seek(SeekFrom::Start(0))?;
    if n >= 4 && magic[..4] == [0x1a, 0x45, 0xdf, 0xa3] {
        return read_matroska(&mut f);
    }
    if n >= 8 {
        read_iso(&mut f, path)
    } else {
        Ok(Found::default())
    }
}

fn read_up_to(r: &mut (impl Read + ?Sized), buf: &mut [u8]) -> Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

fn be16(b: &[u8]) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(..2)?.try_into().ok()?))
}

fn be32(b: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(..4)?.try_into().ok()?))
}

fn be64(b: &[u8]) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(..8)?.try_into().ok()?))
}

/// A box of `kind` around `body`.
fn boxed(kind: [u8; 4], body: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(body.len() + 8);
    b.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
    b.extend_from_slice(&kind);
    b.extend_from_slice(body);
    b
}

// ---- ISO base media (MP4, QuickTime) -------------------------------------

/// A top-level box's place in the file.
#[derive(Clone, Copy, Debug)]
struct Top {
    kind: [u8; 4],
    start: u64,
    /// Header bytes (8 or 16).
    header: u64,
    /// The whole box.
    size: u64,
}

/// The file's top-level boxes (headers only).
fn top_level(f: &mut (impl Read + Seek)) -> Result<Vec<Top>> {
    let len = f.seek(SeekFrom::End(0))?;
    let mut at = 0u64;
    let mut out = Vec::new();
    while at + 8 <= len {
        f.seek(SeekFrom::Start(at))?;
        let mut h = [0u8; 16];
        let n = read_up_to(f, &mut h)?;
        if n < 8 {
            break;
        }
        let small = be32(&h).unwrap_or(0) as u64;
        let kind: [u8; 4] = [h[4], h[5], h[6], h[7]];
        let (size, header) = match small {
            0 => (len - at, 8),
            1 if n >= 16 => (be64(&h[8..]).unwrap_or(0), 16),
            s => (s, 8),
        };
        if size < header || at + size > len {
            // A broken (or still being written) tail.
            break;
        }
        out.push(Top {
            kind,
            start: at,
            header,
            size,
        });
        at += size;
    }
    Ok(out)
}

/// A box in memory: its type, the bytes before its children (all of them
/// for a leaf), its children, and bytes after them no box could be read
/// from (QuickTime's terminators).
#[derive(Clone, Debug, PartialEq)]
struct Node {
    kind: [u8; 4],
    head: Vec<u8>,
    children: Vec<Node>,
    tail: Vec<u8>,
}

impl Node {
    fn leaf(kind: [u8; 4], body: Vec<u8>) -> Self {
        Self {
            kind,
            head: body,
            children: Vec::new(),
            tail: Vec::new(),
        }
    }

    fn size(&self) -> u64 {
        let body = self.head.len() as u64
            + self.children.iter().map(Node::size).sum::<u64>()
            + self.tail.len() as u64;
        if body + 8 > u32::MAX as u64 {
            body + 16
        } else {
            body + 8
        }
    }

    fn write(&self, out: &mut Vec<u8>) {
        let size = self.size();
        if size > u32::MAX as u64 {
            out.extend_from_slice(&1u32.to_be_bytes());
            out.extend_from_slice(&self.kind);
            out.extend_from_slice(&size.to_be_bytes());
        } else {
            out.extend_from_slice(&(size as u32).to_be_bytes());
            out.extend_from_slice(&self.kind);
        }
        out.extend_from_slice(&self.head);
        for c in &self.children {
            c.write(out);
        }
        out.extend_from_slice(&self.tail);
    }

    fn child(&self, kind: &[u8; 4]) -> Option<&Node> {
        self.children.iter().find(|c| &c.kind == kind)
    }

    fn child_mut(&mut self, kind: &[u8; 4]) -> Option<&mut Node> {
        self.children.iter_mut().find(|c| &c.kind == kind)
    }

    /// Its body read as children after `head` bytes.
    fn open(&mut self, head: usize) {
        if !self.children.is_empty() || self.head.len() < head {
            return;
        }
        let body = std::mem::take(&mut self.head);
        let (children, tail) = parse_boxes(&body[head..]);
        self.head = body[..head].to_vec();
        self.children = children;
        self.tail = tail;
    }
}

/// The boxes in `b`, and what is left after the last whole one.
fn parse_boxes(b: &[u8]) -> (Vec<Node>, Vec<u8>) {
    let mut out = Vec::new();
    let mut at = 0usize;
    while b.len() - at >= 8 {
        let small = be32(&b[at..]).unwrap_or(0) as u64;
        let kind: [u8; 4] = [b[at + 4], b[at + 5], b[at + 6], b[at + 7]];
        let (size, header) = match small {
            0 => ((b.len() - at) as u64, 8usize),
            1 => match be64(&b[at + 8..]) {
                Some(s) => (s, 16),
                None => break,
            },
            s => (s, 8),
        };
        if size < header as u64 || size > (b.len() - at) as u64 {
            break;
        }
        let end = at + size as usize;
        out.push(Node::leaf(kind, b[at + header..end].to_vec()));
        at = end;
    }
    (out, b[at..].to_vec())
}

/// Boxes that hold only boxes, on the way to the sample tables.
const CONTAINERS: [&[u8; 4]; 5] = [b"moov", b"trak", b"mdia", b"minf", b"stbl"];

/// Open the containers under `n` down to every track's sample tables and
/// sample descriptions.
fn open_tree(n: &mut Node) {
    for c in &mut n.children {
        if CONTAINERS.contains(&&c.kind) {
            c.open(0);
            open_tree(c);
        } else if &c.kind == b"stsd" {
            // Version and flags, then the entry count.
            c.open(8);
        }
    }
}

/// A track's handler (`vide`, `soun`, …).
fn handler(trak: &Node) -> Option<[u8; 4]> {
    let h = trak.child(b"mdia")?.child(b"hdlr")?;
    h.head.get(8..12)?.try_into().ok()
}

fn sample_entry(trak: &Node) -> Option<&Node> {
    trak.child(b"mdia")?
        .child(b"minf")?
        .child(b"stbl")?
        .child(b"stsd")?
        .children
        .first()
}

fn sample_entry_mut(trak: &mut Node) -> Option<&mut Node> {
    trak.child_mut(b"mdia")?
        .child_mut(b"minf")?
        .child_mut(b"stbl")?
        .child_mut(b"stsd")?
        .children
        .first_mut()
}

/// Bytes of a picture's sample entry before its boxes.
const VISUAL_HEAD: usize = 78;

/// Bytes of a sound's sample entry before its boxes: 28, more in
/// QuickTime's versions 1 (16) and 2 (36).
fn sound_head(entry_body: &[u8], quicktime: bool) -> usize {
    match be16(entry_body.get(8..).unwrap_or(&[])) {
        Some(1) if quicktime => 44,
        Some(2) if quicktime => 64,
        _ => 28,
    }
}

/// The file's major brand is QuickTime's (or it has no `ftyp`).
fn is_quicktime(f: &mut (impl Read + Seek), tops: &[Top]) -> Result<bool> {
    let Some(ftyp) = tops.iter().find(|t| &t.kind == b"ftyp") else {
        return Ok(true);
    };
    f.seek(SeekFrom::Start(ftyp.start + ftyp.header))?;
    let mut brand = [0u8; 4];
    read_up_to(f, &mut brand)?;
    Ok(&brand == b"qt  ")
}

fn read_moov(f: &mut (impl Read + Seek), path: &Path, moov: &Top) -> Result<Node> {
    if moov.size > MAX_MOOV {
        return Err(VideoError::media(path, "the movie's index is too large"));
    }
    f.seek(SeekFrom::Start(moov.start + moov.header))?;
    let mut body = vec![0u8; (moov.size - moov.header) as usize];
    f.read_exact(&mut body)?;
    let mut n = Node::leaf(*b"moov", body);
    n.open(0);
    open_tree(&mut n);
    Ok(n)
}

fn read_iso(f: &mut (impl Read + Seek), path: &Path) -> Result<Found> {
    let tops = top_level(f)?;
    let Some(moov) = tops.iter().find(|t| &t.kind == b"moov").copied() else {
        return Ok(Found::default());
    };
    let quicktime = is_quicktime(f, &tops)?;
    let moov = read_moov(f, path, &moov)?;
    let mut found = Found::default();
    for trak in moov.children.iter().filter(|c| &c.kind == b"trak") {
        match &handler(trak).unwrap_or_default() {
            b"vide" if found.video.is_none() => {
                found.video = sample_entry(trak)
                    .and_then(|e| {
                        let mut e = e.clone();
                        e.open(VISUAL_HEAD);
                        spherical_v2(&e)
                    })
                    .or_else(|| {
                        let entry = sample_entry(trak)?;
                        let coded = (
                            be16(entry.head.get(24..)?)? as u32,
                            be16(entry.head.get(26..)?)? as u32,
                        );
                        trak.children
                            .iter()
                            .filter(|c| &c.kind == b"uuid" && c.head.get(..16) == Some(&V1_UUID))
                            .find_map(|c| {
                                spherical_v1(&String::from_utf8_lossy(&c.head[16..]), coded)
                            })
                    });
            }
            b"soun" => {
                let sa3d = sample_entry(trak).and_then(|e| {
                    let mut e = e.clone();
                    e.open(sound_head(&e.head, quicktime));
                    e.child(b"SA3D")
                        .and_then(|b| SpatialAudio::from_body(&b.head))
                });
                found.audio.push(sa3d);
            }
            _ => {}
        }
    }
    Ok(found)
}

/// Spherical Video V2 in a picture's (opened) sample entry.
fn spherical_v2(entry: &Node) -> Option<Spherical> {
    let mut sv3d = entry.child(b"sv3d")?.clone();
    sv3d.open(0);
    let mut proj = sv3d.child(b"proj")?.clone();
    proj.open(0);
    let fixed =
        |b: &[u8], at: usize| -> Option<f32> { Some(be32(b.get(at..)?)? as i32 as f32 / 65536.0) };
    let pose = proj
        .child(b"prhd")
        .and_then(|p| {
            Some(Pose {
                yaw: fixed(&p.head, 4)?,
                pitch: fixed(&p.head, 8)?,
                roll: fixed(&p.head, 12)?,
            })
        })
        .unwrap_or_default();
    let coded_width = be16(entry.head.get(24..)?)? as f32;
    let stereo = entry
        .child(b"st3d")
        .and_then(|s| s.head.get(4).copied())
        .map_or(StereoMode::Mono, st3d_mode);
    let projection = if let Some(e) = proj.child(b"equi") {
        equi_crop(e.head.get(4..)?)
    } else if let Some(c) = proj.child(b"cbmp") {
        let layout = be32(c.head.get(4..)?)?;
        if layout != 0 {
            tracing::info!("cubemap layout {layout} is not known");
            return None;
        }
        let padding = be32(c.head.get(8..)?)? as f32;
        let (_, _, ew, _) = stereo.left_eye();
        let face = (coded_width * ew / 3.0).max(1.0);
        Projection::Cubemap {
            padding: (padding / face).clamp(0.0, 0.45),
        }
    } else {
        tracing::info!("a mesh projection is not shown as 360°");
        return None;
    };
    Some(Spherical {
        projection,
        stereo,
        pose,
    })
}

/// An `equi` box's bounds (0.32 fixed point shares) as a crop.
fn equi_crop(b: &[u8]) -> Projection {
    let share = |at: usize| be32(b.get(at..).unwrap_or(&[])).unwrap_or(0) as f64 / 4_294_967_296.0;
    Projection::Equirectangular {
        crop: Crop {
            top: share(0),
            bottom: share(4),
            left: share(8),
            right: share(12),
        }
        .clamped(),
    }
}

fn st3d_mode(m: u8) -> StereoMode {
    match m {
        1 => StereoMode::TopBottom,
        2 => StereoMode::LeftRight,
        4 => StereoMode::RightLeft,
        _ => StereoMode::Mono,
    }
}

fn st3d_code(s: StereoMode) -> u8 {
    match s {
        StereoMode::Mono => 0,
        StereoMode::TopBottom => 1,
        StereoMode::LeftRight => 2,
        StereoMode::RightLeft => 4,
    }
}

/// The text between `<GSpherical:name>` and its end tag (any prefix).
fn xml_value<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let open = xml.find(&format!(":{name}>"))?;
    let from = open + name.len() + 2;
    let to = xml[from..].find("</")? + from;
    Some(xml[from..to].trim())
}

/// Spherical Video V1's XML (`coded`: the stored picture's size, for the
/// cropped area's defaults).
pub fn spherical_v1(xml: &str, coded: (u32, u32)) -> Option<Spherical> {
    let truth = |n: &str| xml_value(xml, n).is_some_and(|v| v.eq_ignore_ascii_case("true"));
    if !truth("Spherical") {
        return None;
    }
    if !xml_value(xml, "ProjectionType").is_some_and(|p| p.eq_ignore_ascii_case("equirectangular"))
    {
        return None;
    }
    let stereo = match xml_value(xml, "StereoMode").map(str::to_ascii_lowercase) {
        Some(m) if m == "top-bottom" => StereoMode::TopBottom,
        Some(m) if m == "left-right" => StereoMode::LeftRight,
        _ => StereoMode::Mono,
    };
    let (_, _, ew, eh) = stereo.left_eye();
    let (w, h) = (coded.0 as f64 * ew as f64, coded.1 as f64 * eh as f64);
    let num = |n: &str| xml_value(xml, n).and_then(|v| v.parse::<f64>().ok());
    let full_w = num("FullPanoWidthPixels").unwrap_or(w).max(1.0);
    let full_h = num("FullPanoHeightPixels").unwrap_or(h).max(1.0);
    let crop_w = num("CroppedAreaImageWidthPixels").unwrap_or(w);
    let crop_h = num("CroppedAreaImageHeightPixels").unwrap_or(h);
    let left = num("CroppedAreaLeftPixels").unwrap_or(0.0);
    let top = num("CroppedAreaTopPixels").unwrap_or(0.0);
    let crop = Crop {
        top: top / full_h,
        bottom: 1.0 - (top + crop_h) / full_h,
        left: left / full_w,
        right: 1.0 - (left + crop_w) / full_w,
    }
    .clamped();
    Some(Spherical {
        projection: Projection::Equirectangular { crop },
        stereo,
        pose: Pose::default(),
    })
}

/// The V2 boxes for a picture coded at `coded` pixels: `st3d` and `sv3d`.
pub fn v2_boxes(s: &Spherical, coded: (u32, u32)) -> Vec<u8> {
    let full = |payload: &[u8]| {
        let mut b = vec![0u8; 4];
        b.extend_from_slice(payload);
        b
    };
    let mut out = boxed(*b"st3d", &full(&[st3d_code(s.stereo)]));
    let mut svhd = TOOL.as_bytes().to_vec();
    svhd.push(0);
    let fixed = |d: f32| ((d as f64 * 65536.0).round() as i32).to_be_bytes();
    let mut prhd = Vec::new();
    prhd.extend_from_slice(&fixed(s.pose.yaw));
    prhd.extend_from_slice(&fixed(s.pose.pitch));
    prhd.extend_from_slice(&fixed(s.pose.roll));
    let data = match s.projection {
        Projection::Equirectangular { crop } => {
            let c = crop.clamped();
            let share = |v: f64| (v * 4_294_967_296.0).round().min(u32::MAX as f64) as u32;
            let mut b = Vec::new();
            for v in [c.top, c.bottom, c.left, c.right] {
                b.extend_from_slice(&share(v).to_be_bytes());
            }
            boxed(*b"equi", &full(&b))
        }
        Projection::Cubemap { padding } => {
            let (_, _, ew, _) = s.stereo.left_eye();
            let face = coded.0 as f32 * ew / 3.0;
            let px = (padding.clamp(0.0, 0.45) * face).round() as u32;
            let mut b = 0u32.to_be_bytes().to_vec();
            b.extend_from_slice(&px.to_be_bytes());
            boxed(*b"cbmp", &full(&b))
        }
    };
    let mut proj = boxed(*b"prhd", &full(&prhd));
    proj.extend_from_slice(&data);
    let mut sv3d = boxed(*b"svhd", &full(&svhd));
    sv3d.extend_from_slice(&boxed(*b"proj", &proj));
    out.extend_from_slice(&boxed(*b"sv3d", &sv3d));
    out
}

/// V1's XML for an equirectangular picture coded at `coded` pixels (`None`
/// for a cubemap: V1 has none).
pub fn v1_xml(s: &Spherical, coded: (u32, u32)) -> Option<String> {
    let Projection::Equirectangular { crop } = s.projection else {
        return None;
    };
    let stereo = match s.stereo {
        StereoMode::Mono => "mono",
        StereoMode::TopBottom => "top-bottom",
        StereoMode::LeftRight => "left-right",
        // V1 has no right-left.
        StereoMode::RightLeft => return None,
    };
    let (_, _, ew, eh) = s.stereo.left_eye();
    let (w, h) = (coded.0 as f64 * ew as f64, coded.1 as f64 * eh as f64);
    let c = crop.clamped();
    let full_w = (w / (1.0 - c.left - c.right)).round();
    let full_h = (h / (1.0 - c.top - c.bottom)).round();
    let mut xml = String::from(
        "<?xml version=\"1.0\"?><rdf:SphericalVideo\n\
         xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"\n\
         xmlns:GSpherical=\"http://ns.google.com/videos/1.0/spherical/\">",
    );
    let mut tag = |name: &str, value: String| {
        xml.push_str(&format!("<GSpherical:{name}>{value}</GSpherical:{name}>"));
    };
    tag("Spherical", "true".into());
    tag("Stitched", "true".into());
    tag("StitchingSoftware", TOOL.into());
    tag("ProjectionType", "equirectangular".into());
    tag("StereoMode", stereo.into());
    if !c.is_none() {
        tag("CroppedAreaImageWidthPixels", format!("{}", w.round()));
        tag("CroppedAreaImageHeightPixels", format!("{}", h.round()));
        tag("FullPanoWidthPixels", format!("{full_w}"));
        tag("FullPanoHeightPixels", format!("{full_h}"));
        tag(
            "CroppedAreaLeftPixels",
            format!("{}", (c.left * full_w).round()),
        );
        tag(
            "CroppedAreaTopPixels",
            format!("{}", (c.top * full_h).round()),
        );
    }
    xml.push_str("</rdf:SphericalVideo>");
    Some(xml)
}

/// What to write into a movie.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Metadata {
    /// The picture's 360° layout (V2 boxes, and V1's XML where it can say
    /// it).
    pub video: Option<Spherical>,
    /// The first sound track's spatial audio.
    pub audio: Option<SpatialAudio>,
}

impl Metadata {
    pub fn is_empty(&self) -> bool {
        self.video.is_none() && self.audio.is_none()
    }
}

/// Write `meta` into the MP4 or QuickTime movie `path` (in place: a copy
/// beside it is moved over it). Earlier 360° metadata of the tracks
/// written to is replaced; the media is copied as it is.
pub fn write(path: &Path, meta: &Metadata) -> Result<()> {
    if meta.is_empty() {
        return Ok(());
    }
    let mut f = BufReader::new(File::open(path)?);
    let tops = top_level(&mut f)?;
    if tops.iter().any(|t| &t.kind == b"moof") {
        return Err(VideoError::media(
            path,
            "360° metadata cannot be written into a fragmented movie",
        ));
    }
    let Some(top) = tops.iter().find(|t| &t.kind == b"moov").copied() else {
        return Err(VideoError::media(path, "not an MP4 or QuickTime movie"));
    };
    let quicktime = is_quicktime(&mut f, &tops)?;
    let mut moov = read_moov(&mut f, path, &top)?;
    let mut wrote_video = meta.video.is_none();
    let mut wrote_audio = meta.audio.is_none();
    for trak in moov.children.iter_mut().filter(|c| &c.kind == b"trak") {
        match &handler(trak).unwrap_or_default() {
            b"vide" if !wrote_video => {
                if let Some(s) = &meta.video {
                    wrote_video = put_video(trak, s);
                }
            }
            b"soun" if !wrote_audio => {
                if let Some(a) = &meta.audio {
                    wrote_audio = put_audio(trak, a, quicktime);
                }
            }
            _ => {}
        }
    }
    if !wrote_video {
        return Err(VideoError::media(path, "no picture to mark as 360°"));
    }
    if !wrote_audio {
        return Err(VideoError::media(path, "no sound to mark as ambisonic"));
    }
    // The media after the index moves by what it grew.
    let old_end = top.start + top.size;
    let delta = moov.size() as i64 - top.size as i64;
    if delta != 0 {
        shift_chunk_offsets(&mut moov, old_end, delta).map_err(|e| VideoError::media(path, e))?;
    }
    let mut bytes = Vec::with_capacity(moov.size() as usize);
    moov.write(&mut bytes);

    let tmp = path.with_extension("360.partial");
    let result = (|| -> Result<()> {
        let mut out = std::io::BufWriter::new(File::create(&tmp)?);
        f.seek(SeekFrom::Start(0))?;
        std::io::copy(&mut (&mut f).take(top.start), &mut out)?;
        out.write_all(&bytes)?;
        f.seek(SeekFrom::Start(old_end))?;
        std::io::copy(&mut f, &mut out)?;
        out.flush()?;
        out.get_ref().sync_all()?;
        Ok(())
    })();
    drop(f);
    match result {
        Ok(()) => {
            std::fs::rename(&tmp, path)?;
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// The picture's track marked as `s`: V2 boxes in its sample entry
/// (before the optional `clap`/`pasp`), V1's XML at the track's end.
fn put_video(trak: &mut Node, s: &Spherical) -> bool {
    let Some(entry) = sample_entry_mut(trak) else {
        return false;
    };
    entry.open(VISUAL_HEAD);
    let coded = (
        be16(entry.head.get(24..).unwrap_or(&[])).unwrap_or(0) as u32,
        be16(entry.head.get(26..).unwrap_or(&[])).unwrap_or(0) as u32,
    );
    entry
        .children
        .retain(|c| &c.kind != b"st3d" && &c.kind != b"sv3d");
    let (boxes, _) = parse_boxes(&v2_boxes(s, coded));
    let at = entry
        .children
        .iter()
        .position(|c| &c.kind == b"clap" || &c.kind == b"pasp")
        .unwrap_or(entry.children.len());
    entry.children.splice(at..at, boxes);
    trak.children
        .retain(|c| !(&c.kind == b"uuid" && c.head.get(..16) == Some(&V1_UUID)));
    if let Some(xml) = v1_xml(s, coded) {
        let mut body = V1_UUID.to_vec();
        body.extend_from_slice(xml.as_bytes());
        trak.children.push(Node::leaf(*b"uuid", body));
    }
    true
}

/// A sound track marked as `a` (`SA3D` at the end of its sample entry).
fn put_audio(trak: &mut Node, a: &SpatialAudio, quicktime: bool) -> bool {
    let Some(entry) = sample_entry_mut(trak) else {
        return false;
    };
    let head = sound_head(&entry.head, quicktime);
    entry.open(head);
    entry.children.retain(|c| &c.kind != b"SA3D");
    // Opus says an ambisonic field by its channel mapping family (RFC
    // 8486): the discrete family's table, read as ACN components.
    if &entry.kind == b"Opus"
        && a.field().is_some()
        && let Some(dops) = entry.child_mut(b"dOps")
        && dops.head.get(10) == Some(&255)
    {
        dops.head[10] = 2;
    }
    let (boxes, _) = parse_boxes(&a.to_box());
    entry.children.extend(boxes);
    true
}

/// Every chunk offset at or after `from` (the media stored after the
/// index) moved by `delta`.
fn shift_chunk_offsets(moov: &mut Node, from: u64, delta: i64) -> std::result::Result<(), String> {
    for trak in moov.children.iter_mut().filter(|c| &c.kind == b"trak") {
        let Some(stbl) = trak
            .child_mut(b"mdia")
            .and_then(|m| m.child_mut(b"minf"))
            .and_then(|m| m.child_mut(b"stbl"))
        else {
            continue;
        };
        for t in stbl.children.iter_mut() {
            let wide = match &t.kind {
                b"stco" => false,
                b"co64" => true,
                _ => continue,
            };
            let n = be32(t.head.get(4..).unwrap_or(&[])).unwrap_or(0) as usize;
            let w = if wide { 8 } else { 4 };
            if t.head.len() < 8 + n * w {
                return Err("a broken chunk offset table".into());
            }
            for i in 0..n {
                let at = 8 + i * w;
                let v = if wide {
                    be64(&t.head[at..]).unwrap_or(0)
                } else {
                    be32(&t.head[at..]).unwrap_or(0) as u64
                };
                if v < from {
                    continue;
                }
                let moved = v as i64 + delta;
                if wide {
                    t.head[at..at + 8].copy_from_slice(&(moved as u64).to_be_bytes());
                } else {
                    let m = u32::try_from(moved)
                        .map_err(|_| "the movie is too large to grow its index".to_string())?;
                    t.head[at..at + 4].copy_from_slice(&m.to_be_bytes());
                }
            }
        }
    }
    Ok(())
}

// ---- Matroska / WebM ----------------------------------------------------

mod ebml {
    pub const SEGMENT: u32 = 0x1853_8067;
    pub const TRACKS: u32 = 0x1654_AE6B;
    pub const TAGS: u32 = 0x1254_C367;
    pub const TRACK_ENTRY: u32 = 0xAE;
    pub const TRACK_TYPE: u32 = 0x83;
    pub const TRACK_UID: u32 = 0x73C5;
    pub const VIDEO: u32 = 0xE0;
    pub const PIXEL_WIDTH: u32 = 0xB0;
    pub const STEREO_MODE: u32 = 0x53B8;
    pub const PROJECTION: u32 = 0x7670;
    pub const PROJECTION_TYPE: u32 = 0x7671;
    pub const PROJECTION_PRIVATE: u32 = 0x7672;
    pub const POSE_YAW: u32 = 0x7673;
    pub const POSE_PITCH: u32 = 0x7674;
    pub const POSE_ROLL: u32 = 0x7675;
    pub const TAG: u32 = 0x7373;
    pub const TARGETS: u32 = 0x63C0;
    pub const TAG_TRACK_UID: u32 = 0x63C5;
    pub const SIMPLE_TAG: u32 = 0x67C8;
    pub const TAG_NAME: u32 = 0x45A3;
    pub const TAG_STRING: u32 = 0x4487;
    pub const PIXEL_HEIGHT: u32 = 0xBA;
}

/// An element ID (marker kept) and its length in bytes.
fn ebml_id(b: &[u8]) -> Option<(u32, usize)> {
    let first = *b.first()?;
    let len = first.leading_zeros() as usize + 1;
    if len > 4 || b.len() < len {
        return None;
    }
    Some((b[..len].iter().fold(0u32, |v, x| v << 8 | *x as u32), len))
}

/// A size (`None`: unknown) and its length in bytes.
fn ebml_size(b: &[u8]) -> Option<(Option<u64>, usize)> {
    let first = *b.first()?;
    let len = first.leading_zeros() as usize + 1;
    if len > 8 || b.len() < len {
        return None;
    }
    let mask = if len == 8 { 0 } else { 0xffu8 >> len };
    let mut v = (first & mask) as u64;
    let mut ones = (first & mask) == mask;
    for x in &b[1..len] {
        v = v << 8 | *x as u64;
        ones &= *x == 0xff;
    }
    Some((if ones { None } else { Some(v) }, len))
}

/// The elements in `b`: (ID, body).
fn ebml_children(b: &[u8]) -> Vec<(u32, &[u8])> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < b.len() {
        let Some((id, il)) = ebml_id(&b[at..]) else {
            break;
        };
        let Some((size, sl)) = ebml_size(&b[at + il..]) else {
            break;
        };
        let from = at + il + sl;
        let to = size.map_or(b.len(), |s| from.saturating_add(s as usize).min(b.len()));
        out.push((id, &b[from..to]));
        at = to;
    }
    out
}

fn ebml_uint(b: &[u8]) -> u64 {
    b.iter().take(8).fold(0u64, |v, x| v << 8 | *x as u64)
}

fn ebml_float(b: &[u8]) -> f32 {
    match b.len() {
        4 => f32::from_be_bytes([b[0], b[1], b[2], b[3]]),
        8 => f64::from_be_bytes(b.try_into().unwrap_or_default()) as f32,
        _ => 0.0,
    }
}

/// An element's header at `at`: its ID, size and header length.
fn ebml_header(f: &mut dyn ReadSeek, at: u64) -> Result<Option<(u32, Option<u64>, u64)>> {
    f.seek(SeekFrom::Start(at))?;
    let mut h = [0u8; 12];
    let n = read_up_to(f, &mut h)?;
    let Some((id, il)) = ebml_id(&h[..n]) else {
        return Ok(None);
    };
    let Some((size, sl)) = ebml_size(&h[il..n]) else {
        return Ok(None);
    };
    Ok(Some((id, size, (il + sl) as u64)))
}

fn read_matroska(f: &mut dyn ReadSeek) -> Result<Found> {
    let len = f.seek(SeekFrom::End(0))?;
    // The EBML header, then the segment.
    let Some((_, Some(size), hl)) = ebml_header(f, 0)? else {
        return Ok(Found::default());
    };
    let mut at = hl + size;
    let mut segment = None;
    while at < len {
        let Some((id, size, hl)) = ebml_header(f, at)? else {
            break;
        };
        if id == ebml::SEGMENT {
            segment = Some((at + hl, size.map_or(len, |s| (at + hl + s).min(len))));
            break;
        }
        let Some(size) = size else { break };
        at += hl + size;
    }
    let Some((mut at, end)) = segment else {
        return Ok(Found::default());
    };
    let mut tracks = None;
    let mut tags = Vec::new();
    while at < end {
        let Some((id, size, hl)) = ebml_header(f, at)? else {
            break;
        };
        let Some(size) = size else {
            // A cluster of unknown size (a live stream): what is after it
            // cannot be found without reading it all.
            break;
        };
        if (id == ebml::TRACKS || id == ebml::TAGS) && size <= MAX_ELEMENT {
            let mut body = vec![0u8; size as usize];
            f.seek(SeekFrom::Start(at + hl))?;
            f.read_exact(&mut body)?;
            if id == ebml::TRACKS {
                tracks.get_or_insert(body);
            } else {
                tags.push(body);
            }
        }
        // Tags come after the clusters in most files: on, header by
        // header.
        at += hl + size;
    }
    let Some(tracks) = tracks else {
        return Ok(Found::default());
    };
    let mut found = Found::default();
    for (id, entry) in ebml_children(&tracks) {
        if id != ebml::TRACK_ENTRY {
            continue;
        }
        let fields = ebml_children(entry);
        let get = |want: u32| fields.iter().find(|(i, _)| *i == want).map(|(_, b)| *b);
        match get(ebml::TRACK_TYPE).map(ebml_uint) {
            Some(1) if found.video.is_none() => {
                let video = get(ebml::VIDEO).map(ebml_children).unwrap_or_default();
                let vget = |want: u32| video.iter().find(|(i, _)| *i == want).map(|(_, b)| *b);
                let stereo = match vget(ebml::STEREO_MODE).map(ebml_uint) {
                    Some(1) => StereoMode::LeftRight,
                    Some(2) | Some(3) => StereoMode::TopBottom,
                    Some(11) => StereoMode::RightLeft,
                    _ => StereoMode::Mono,
                };
                let coded = (
                    vget(ebml::PIXEL_WIDTH).map_or(0, ebml_uint) as u32,
                    vget(ebml::PIXEL_HEIGHT).map_or(0, ebml_uint) as u32,
                );
                found.video = vget(ebml::PROJECTION)
                    .and_then(|p| matroska_projection(p, stereo, coded))
                    .or_else(|| {
                        let uid = get(ebml::TRACK_UID).map(ebml_uint);
                        tags.iter()
                            .find_map(|t| matroska_v1_tag(t, uid))
                            .and_then(|xml| spherical_v1(&xml, coded))
                    });
            }
            Some(2) => found.audio.push(None),
            _ => {}
        }
    }
    Ok(found)
}

/// Read + Seek as one object (for a closure over the reader).
trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

/// V2's `Projection` element.
fn matroska_projection(p: &[u8], stereo: StereoMode, coded: (u32, u32)) -> Option<Spherical> {
    let fields = ebml_children(p);
    let get = |want: u32| fields.iter().find(|(i, _)| *i == want).map(|(_, b)| *b);
    let private = get(ebml::PROJECTION_PRIVATE).unwrap_or(&[]);
    let projection = match get(ebml::PROJECTION_TYPE).map_or(0, ebml_uint) {
        // The private data is the box's body (version and flags first).
        1 => equi_crop(private.get(4..).unwrap_or(&[])),
        2 => {
            let layout = be32(private.get(4..)?)?;
            if layout != 0 {
                return None;
            }
            let padding = be32(private.get(8..)?)? as f32;
            let (_, _, ew, _) = stereo.left_eye();
            let face = (coded.0 as f32 * ew / 3.0).max(1.0);
            Projection::Cubemap {
                padding: (padding / face).clamp(0.0, 0.45),
            }
        }
        _ => return None,
    };
    let pose = Pose {
        yaw: get(ebml::POSE_YAW).map_or(0.0, ebml_float),
        pitch: get(ebml::POSE_PITCH).map_or(0.0, ebml_float),
        roll: get(ebml::POSE_ROLL).map_or(0.0, ebml_float),
    };
    Some(Spherical {
        projection,
        stereo,
        pose,
    })
}

/// V1's XML from a `Tags` element: the `spherical-video` tag of the track
/// `uid` (or of no track).
fn matroska_v1_tag(tags: &[u8], uid: Option<u64>) -> Option<String> {
    for (id, tag) in ebml_children(tags) {
        if id != ebml::TAG {
            continue;
        }
        let parts = ebml_children(tag);
        let target = parts
            .iter()
            .find(|(i, _)| *i == ebml::TARGETS)
            .and_then(|(_, t)| {
                ebml_children(t)
                    .into_iter()
                    .find(|(i, _)| *i == ebml::TAG_TRACK_UID)
                    .map(|(_, b)| ebml_uint(b))
            });
        if target.is_some() && uid.is_some() && target != uid {
            continue;
        }
        for (id, simple) in &parts {
            if *id != ebml::SIMPLE_TAG {
                continue;
            }
            let s = ebml_children(simple);
            let name = s.iter().find(|(i, _)| *i == ebml::TAG_NAME);
            let value = s.iter().find(|(i, _)| *i == ebml::TAG_STRING);
            if let (Some((_, n)), Some((_, v))) = (name, value)
                && String::from_utf8_lossy(n).eq_ignore_ascii_case("spherical-video")
            {
                return Some(String::from_utf8_lossy(v).into_owned());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal QuickTime-like movie: `ftyp`, a `moov` with one picture
    /// track and one sound track (each with a chunk offset table), then
    /// `mdat` — the index first, as a movie made to play while it
    /// downloads.
    fn movie(quicktime: bool, sound_version: u16) -> (Vec<u8>, u64) {
        let full = |b: &[u8]| {
            let mut v = vec![0u8; 4];
            v.extend_from_slice(b);
            v
        };
        let hdlr = |h: &[u8; 4]| {
            let mut b = vec![0u8; 4];
            b.extend_from_slice(b"mhlr");
            b.extend_from_slice(h);
            b.extend_from_slice(&[0u8; 12]);
            boxed(*b"hdlr", &b)
        };
        let stco = |offsets: &[u32]| {
            let mut b = vec![0u8; 4];
            b.extend_from_slice(&(offsets.len() as u32).to_be_bytes());
            for o in offsets {
                b.extend_from_slice(&o.to_be_bytes());
            }
            boxed(*b"stco", &b)
        };
        let trak = |handler: &[u8; 4], entry: Vec<u8>, offsets: &[u32]| {
            let mut stsd = full(&1u32.to_be_bytes());
            stsd.extend_from_slice(&entry);
            let mut stbl = boxed(*b"stsd", &stsd);
            stbl.extend_from_slice(&stco(offsets));
            let minf = boxed(*b"stbl", &stbl);
            let mut mdia = boxed(*b"mdhd", &[0u8; 24]);
            mdia.extend_from_slice(&hdlr(handler));
            mdia.extend_from_slice(&boxed(*b"minf", &minf));
            let mut t = boxed(*b"tkhd", &[0u8; 84]);
            t.extend_from_slice(&boxed(*b"mdia", &mdia));
            boxed(*b"trak", &t)
        };
        // A picture's entry: 3840×1920, a pasp after its avcC.
        let mut v = vec![0u8; VISUAL_HEAD];
        v[24..26].copy_from_slice(&3840u16.to_be_bytes());
        v[26..28].copy_from_slice(&1920u16.to_be_bytes());
        v.extend_from_slice(&boxed(*b"avcC", &[1, 2, 3, 4]));
        v.extend_from_slice(&boxed(*b"pasp", &[0, 0, 0, 1, 0, 0, 0, 1]));
        let video = boxed(*b"avc1", &v);
        let mut s = vec![0u8; 28];
        s[8..10].copy_from_slice(&sound_version.to_be_bytes());
        s[16..18].copy_from_slice(&4u16.to_be_bytes());
        if quicktime && sound_version == 1 {
            s.extend_from_slice(&[0u8; 16]);
        }
        if quicktime && sound_version == 2 {
            s.extend_from_slice(&[0u8; 36]);
        }
        s.extend_from_slice(&boxed(*b"chan", &[0u8; 12]));
        let sound = boxed(*b"lpcm", &s);
        let ftyp = boxed(
            *b"ftyp",
            if quicktime {
                b"qt  \0\0\0\0qt  "
            } else {
                b"isom\0\0\0\0isom"
            },
        );
        // Offsets are filled in once the size is known.
        let make = |base: u32| {
            let mut moov = trak(b"vide", video.clone(), &[base, base + 100]);
            moov.extend_from_slice(&trak(b"soun", sound.clone(), &[base + 50]));
            boxed(*b"moov", &moov)
        };
        let base = (ftyp.len() + make(0).len() + 8) as u32;
        let mut file = ftyp;
        file.extend_from_slice(&make(base));
        let mdat_at = file.len() as u64;
        file.extend_from_slice(&boxed(*b"mdat", &[7u8; 200]));
        (file, mdat_at)
    }

    fn offsets(file: &[u8]) -> Vec<u64> {
        let mut c = std::io::Cursor::new(file);
        let tops = top_level(&mut c).unwrap();
        let top = tops.iter().find(|t| &t.kind == b"moov").unwrap();
        let moov = read_moov(&mut c, Path::new("x"), top).unwrap();
        let mut out = Vec::new();
        for trak in moov.children.iter().filter(|c| &c.kind == b"trak") {
            let stbl = trak
                .child(b"mdia")
                .unwrap()
                .child(b"minf")
                .unwrap()
                .child(b"stbl")
                .unwrap();
            let stco = stbl.child(b"stco").unwrap();
            let n = be32(&stco.head[4..]).unwrap() as usize;
            for i in 0..n {
                out.push(be32(&stco.head[8 + 4 * i..]).unwrap() as u64);
            }
        }
        out
    }

    /// A file of `bytes` in the temporary folder (the test removes it).
    fn temp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("ff-spherical-{}-{name}", std::process::id()));
        std::fs::write(&p, bytes).unwrap();
        p
    }

    /// Written boxes are read back; the media after the index keeps its
    /// bytes and every chunk offset still points at them.
    #[test]
    fn metadata_is_written_into_a_movie_and_read_back() {
        for (quicktime, version) in [(true, 0), (true, 1), (true, 2), (false, 0), (false, 1)] {
            let (file, mdat_at) = movie(quicktime, version);
            let before = offsets(&file);
            let p = temp(&format!("m-{quicktime}-{version}.mov"), &file);
            assert_eq!(
                read(&p).unwrap(),
                Found {
                    video: None,
                    audio: vec![None],
                }
            );
            let s = Spherical {
                projection: Projection::Equirectangular {
                    crop: Crop {
                        top: 0.125,
                        ..Crop::default()
                    },
                },
                stereo: StereoMode::TopBottom,
                pose: Pose {
                    yaw: 12.5,
                    pitch: -3.0,
                    roll: 1.0,
                },
            };
            let meta = Metadata {
                video: Some(s),
                audio: Some(SpatialAudio::ambix(AmbiOrder::First)),
            };
            write(&p, &meta).unwrap();
            let found = read(&p).unwrap();
            let v = found.video.unwrap();
            assert_eq!(v.stereo, StereoMode::TopBottom);
            assert!((v.pose.yaw - 12.5).abs() < 1e-4 && (v.pose.pitch + 3.0).abs() < 1e-4);
            let Projection::Equirectangular { crop } = v.projection else {
                panic!("{v:?}")
            };
            assert!((crop.top - 0.125).abs() < 1e-9);
            assert_eq!(
                found.audio,
                vec![Some(SpatialAudio::ambix(AmbiOrder::First))]
            );
            assert_eq!(
                found.audio[0].as_ref().unwrap().field(),
                Some(AmbiOrder::First)
            );
            let after = std::fs::read(&p).unwrap();
            let grown = after.len() as u64 - file.len() as u64;
            assert!(grown > 0);
            // The media moved by what the index grew, untouched.
            assert_eq!(
                &after[(mdat_at + grown) as usize..],
                &file[mdat_at as usize..]
            );
            let moved: Vec<u64> = before.iter().map(|o| o + grown).collect();
            assert_eq!(offsets(&after), moved, "{quicktime} {version}");
            // The V2 boxes sit before the pasp, after the avcC.
            let i = |k: &[u8]| after.windows(4).position(|w| w == k).unwrap();
            assert!(i(b"avcC") < i(b"st3d") && i(b"sv3d") < i(b"pasp"));
            // V1 is there too.
            assert!(after.windows(16).any(|w| w == V1_UUID));
            // Written again: replaced, not added.
            write(&p, &meta).unwrap();
            let again = std::fs::read(&p).unwrap();
            assert_eq!(again.len(), after.len());
            assert_eq!(again.windows(4).filter(|w| w == b"sv3d").count(), 1);
            assert_eq!(again.windows(4).filter(|w| w == b"SA3D").count(), 1);
            let _ = std::fs::remove_file(&p);
        }
    }

    /// A movie whose index comes after its media: nothing moves.
    #[test]
    fn an_index_at_the_end_moves_no_media() {
        let (file, _) = movie(true, 0);
        // ftyp, mdat, moov (offsets as they were: before the index).
        let mut c = std::io::Cursor::new(&file);
        let tops = top_level(&mut c).unwrap();
        let (ftyp, moov, mdat) = (tops[0], tops[1], tops[2]);
        let cut = |t: Top| &file[t.start as usize..(t.start + t.size) as usize];
        let mut reordered = cut(ftyp).to_vec();
        reordered.extend_from_slice(cut(mdat));
        reordered.extend_from_slice(cut(moov));
        let before = offsets(&reordered);
        let p = temp("end.mov", &reordered);
        write(
            &p,
            &Metadata {
                video: Some(Spherical::CUBEMAP),
                audio: None,
            },
        )
        .unwrap();
        let after = std::fs::read(&p).unwrap();
        assert_eq!(offsets(&after), before);
        assert_eq!(read(&p).unwrap().video, Some(Spherical::CUBEMAP));
        // A cubemap has no V1.
        assert!(!after.windows(16).any(|w| w == V1_UUID));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn cubemap_padding_is_a_share_of_a_face() {
        let s = Spherical {
            projection: Projection::Cubemap { padding: 0.0125 },
            ..Spherical::CUBEMAP
        };
        // 3840 wide: faces of 1280, 16 pixels.
        let b = v2_boxes(&s, (3840, 2560));
        let i = b.windows(4).position(|w| w == b"cbmp").unwrap();
        assert_eq!(be32(&b[i + 8..]).unwrap(), 0);
        assert_eq!(be32(&b[i + 12..]).unwrap(), 16);
    }

    #[test]
    fn version_1_xml_is_read_with_its_crop() {
        let xml = r#"<rdf:SphericalVideo xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:GSpherical="http://ns.google.com/videos/1.0/spherical/">
          <GSpherical:Spherical>true</GSpherical:Spherical>
          <GSpherical:Stitched>true</GSpherical:Stitched>
          <GSpherical:ProjectionType>equirectangular</GSpherical:ProjectionType>
          <GSpherical:CroppedAreaImageWidthPixels>1920</GSpherical:CroppedAreaImageWidthPixels>
          <GSpherical:CroppedAreaImageHeightPixels>720</GSpherical:CroppedAreaImageHeightPixels>
          <GSpherical:FullPanoWidthPixels>1920</GSpherical:FullPanoWidthPixels>
          <GSpherical:FullPanoHeightPixels>960</GSpherical:FullPanoHeightPixels>
          <GSpherical:CroppedAreaLeftPixels>0</GSpherical:CroppedAreaLeftPixels>
          <GSpherical:CroppedAreaTopPixels>120</GSpherical:CroppedAreaTopPixels>
        </rdf:SphericalVideo>"#;
        let s = spherical_v1(xml, (1920, 720)).unwrap();
        let Projection::Equirectangular { crop } = s.projection else {
            panic!()
        };
        assert!((crop.top - 0.125).abs() < 1e-9 && (crop.bottom - 0.125).abs() < 1e-9);
        assert_eq!(crop.left, 0.0);
        // What we write reads back the same.
        let ours = v1_xml(&s, (1920, 720)).unwrap();
        assert_eq!(spherical_v1(&ours, (1920, 720)), Some(s));
        assert!(spherical_v1(&xml.replace(">true<", ">false<"), (1920, 720)).is_none());
    }

    /// Matroska: a `Projection` in the track's `Video` element.
    #[test]
    fn a_matroska_projection_is_read() {
        fn el(id: u32, body: &[u8]) -> Vec<u8> {
            let mut b = id
                .to_be_bytes()
                .iter()
                .skip_while(|x| **x == 0)
                .copied()
                .collect::<Vec<_>>();
            // An 8-byte size, as muxers write them.
            b.push(0x01);
            b.extend_from_slice(&(body.len() as u64).to_be_bytes()[1..]);
            b.extend_from_slice(body);
            b
        }
        let mut private = vec![0u8; 4];
        private.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let mut proj = el(ebml::PROJECTION_TYPE, &[1]);
        proj.extend(el(ebml::PROJECTION_PRIVATE, &private));
        proj.extend(el(ebml::POSE_YAW, &30f32.to_be_bytes()));
        let mut video = el(ebml::PIXEL_WIDTH, &[0x0f, 0x00]);
        video.extend(el(ebml::STEREO_MODE, &[3]));
        video.extend(el(ebml::PROJECTION, &proj));
        let mut entry = el(ebml::TRACK_TYPE, &[1]);
        entry.extend(el(ebml::VIDEO, &video));
        let mut audio = el(ebml::TRACK_TYPE, &[2]);
        audio.extend(el(0xE1, &[]));
        let mut tracks = el(ebml::TRACK_ENTRY, &entry);
        tracks.extend(el(ebml::TRACK_ENTRY, &audio));
        let mut segment = el(0x1549_A966, &[0; 8]);
        segment.extend(el(ebml::TRACKS, &tracks));
        // A cluster.
        segment.extend(el(0x1F43_B675, &[0; 32]));
        let mut file = el(0x1A45_DFA3, &[0x42, 0x82, 0x84, b'w', b'e', b'b', b'm']);
        file.extend(el(ebml::SEGMENT, &segment));
        let p = temp("p.webm", &file);
        let found = read(&p).unwrap();
        let v = found.video.unwrap();
        assert_eq!(v.stereo, StereoMode::TopBottom);
        assert_eq!(v.pose.yaw, 30.0);
        assert_eq!(
            v.projection,
            Projection::Equirectangular {
                crop: Crop::default()
            }
        );
        assert_eq!(found.audio, vec![None]);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn spatial_audio_reads_back_and_knows_a_plain_field() {
        let a = SpatialAudio::ambix(AmbiOrder::Second);
        let b = a.to_box();
        assert_eq!(&b[4..8], b"SA3D");
        assert_eq!(SpatialAudio::from_body(&b[8..]), Some(a.clone()));
        assert_eq!(a.field(), Some(AmbiOrder::Second));
        // FuMa-like order of components, or head-locked stereo: no plain
        // field.
        let shuffled = SpatialAudio {
            map: vec![0, 3, 1, 2],
            ..SpatialAudio::ambix(AmbiOrder::First)
        };
        assert_eq!(shuffled.field(), None);
        let with_stereo = SpatialAudio {
            map: (0..6).collect(),
            head_locked_stereo: true,
            ..SpatialAudio::ambix(AmbiOrder::First)
        };
        let back = SpatialAudio::from_body(&with_stereo.to_box()[8..]).unwrap();
        assert!(back.head_locked_stereo && back.field().is_none());
    }
}
