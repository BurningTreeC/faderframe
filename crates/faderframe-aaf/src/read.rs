//! Reading the sound of an AAF file (Media Composer, Pro Tools, Premiere,
//! Resolve exports, and ours) into a [`Composition`]: the top-level
//! composition's sound slots flattened into clips (sequences, fillers,
//! transitions → fades and crossfades, gain operations → clip gain and its
//! envelope, selectors, essence groups and nested compositions followed),
//! each clip's source followed through master mobs to the file source mob
//! that holds the sound — embedded in the file, linked by a locator, or
//! missing —, the timecode the composition starts at and its markers.
//!
//! Classes the file defines itself are known by the parents its meta
//! dictionary gives them, extension properties by the local ids it gives
//! them.

use crate::cfb::CfbError;
use crate::ids::{Auid, MobId};
use crate::model::{self, cls, defs, pid};
use crate::store::read::{ReadError, StoreReader};
use crate::store::{Object, Rational, Stream, Value, decode_string};
use crate::timeline::{
    Clip, Composition, Embedded, Essence, EssenceFormat, Fade, FadeCurve, Marker, Media, Timecode,
    Track,
};
use std::collections::{BTreeSet, HashMap};
use std::io::{Read, Seek, Write};

/// An AAF file open for reading.
pub struct AafFile<R: Read + Seek> {
    store: StoreReader<R>,
    root: Object,
}

/// What was read, and what was left out.
#[derive(Clone, Debug, Default)]
pub struct Imported {
    pub composition: Composition,
    pub notes: Vec<String>,
}

impl AafFile<std::fs::File> {
    pub fn open_path(path: &std::path::Path) -> Result<Self, ReadError> {
        let f = std::fs::File::open(path).map_err(CfbError::from)?;
        match AafFile::open(f) {
            Err(ReadError::Cfb(CfbError::NotCompound)) if is_omf(path) => Err(ReadError::Omf),
            r => r,
        }
    }
}

/// An OMF file: a Bento container (its label, at the end, carries the
/// magic `¤CM¥Hdr`), or named so.
pub fn is_omf(path: &std::path::Path) -> bool {
    use std::io::{Seek, SeekFrom};
    let named = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("omf") || e.eq_ignore_ascii_case("omfi"));
    let tail = (|| -> std::io::Result<Vec<u8>> {
        let mut f = std::fs::File::open(path)?;
        let len = f.seek(SeekFrom::End(0))?;
        f.seek(SeekFrom::Start(len.saturating_sub(64)))?;
        let mut b = Vec::new();
        f.read_to_end(&mut b)?;
        Ok(b)
    })()
    .unwrap_or_default();
    named || tail.windows(7).any(|w| w == b"\xa4CM\xa5Hdr")
}

impl<R: Read + Seek> AafFile<R> {
    pub fn open(file: R) -> Result<Self, ReadError> {
        let mut store = StoreReader::open(file)?;
        let root = store.read_root()?;
        if root.strong(pid::ROOT_HEADER).is_none() {
            return Err(ReadError::NotAaf("there is no header"));
        }
        Ok(AafFile { store, root })
    }

    /// The whole object tree (tests, dumps).
    pub fn root(&self) -> &Object {
        &self.root
    }

    /// The top-level composition's sound at `rate` samples per second.
    pub fn composition(&mut self, rate: u32) -> Result<Imported, ReadError> {
        let mut r = Reader::new(&self.root, rate);
        let comp = r
            .top_level()
            .ok_or(ReadError::NotAaf("there is no composition"))?;
        let mut composition = r.composition(comp);
        // Embedded WAVE and AIFC essence may be whole files.
        for m in &mut composition.media {
            for e in &mut m.essence {
                if let Essence::Embedded(emb) = e {
                    let mut head = Vec::new();
                    let mut sniff = Sniff {
                        out: &mut head,
                        left: 12,
                    };
                    let _ = self.store.cfb.copy_to(emb.entry, &mut sniff, &mut |_| {});
                    if head.starts_with(b"RIFF") || head.starts_with(b"RF64") {
                        emb.format = EssenceFormat::Wave;
                    } else if head.starts_with(b"FORM") {
                        emb.format = EssenceFormat::Aiff;
                    }
                }
            }
        }
        Ok(Imported {
            composition,
            notes: r.notes.into_iter().collect(),
        })
    }

    /// Copy an embedded essence stream into `to`.
    pub fn copy_essence(
        &mut self,
        e: &Embedded,
        to: &mut dyn Write,
        progress: &mut dyn FnMut(u64),
    ) -> Result<(), ReadError> {
        Ok(self.store.cfb.copy_to(e.entry, to, progress)?)
    }
}

/// Takes the first bytes and stops.
struct Sniff<'a> {
    out: &'a mut Vec<u8>,
    left: usize,
}

impl Write for Sniff<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.left == 0 {
            return Err(std::io::Error::other("enough"));
        }
        let n = buf.len().min(self.left);
        self.out.extend_from_slice(&buf[..n]);
        self.left -= n;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A transition in a sequence: the segment after it, its length (edit
/// units), its Level points.
type Between = (usize, i64, Vec<(f64, f64)>);

/// A clip found while flattening, before its fades are known.
#[derive(Clone, Debug)]
struct Piece {
    clip: Clip,
}

struct Reader<'a> {
    rate: u32,
    mobs: HashMap<MobId, &'a Object>,
    essence: HashMap<MobId, (usize, u64)>,
    /// Classes the file defines: id → parent.
    parents: HashMap<Auid, Auid>,
    /// Local ids the file gives properties.
    pids: HashMap<Auid, u16>,
    operations: HashMap<Auid, String>,
    media: Vec<Media>,
    media_of: HashMap<MobId, usize>,
    notes: BTreeSet<String>,
}

/// Units at `er` → samples at `rate` (rounded).
fn rescale(units: i64, er: Rational, rate: u32) -> i64 {
    if er.num <= 0 || er.den <= 0 {
        return units;
    }
    let n = i128::from(units) * i128::from(rate) * i128::from(er.den);
    let d = i128::from(er.num);
    ((2 * n + d).div_euclid(2 * d)) as i64
}

/// Units at `from` → units at `to`.
fn convert(units: i64, from: Rational, to: Rational) -> i64 {
    if from == to || from.num <= 0 || to.num <= 0 || from.den <= 0 || to.den <= 0 {
        return units;
    }
    let n = i128::from(units) * i128::from(to.num) * i128::from(from.den);
    let d = i128::from(to.den) * i128::from(from.num);
    ((2 * n + d).div_euclid(2 * d)) as i64
}

/// An indirect value as a number (rationals and integers).
fn indirect_number(b: &[u8]) -> Option<f64> {
    if b.first() != Some(&0x4c) {
        return None;
    }
    let ty = Auid::from_le(b.get(1..17)?)?;
    let v = b.get(17..)?;
    if ty == model::ty::RATIONAL {
        return Rational::decode(v).map(Rational::to_f64);
    }
    match model::type_def(ty).map(|t| &t.kind) {
        Some(model::TypeKind::Int { size, signed }) => {
            let mut raw = [0u8; 8];
            let n = usize::from(*size).min(8);
            raw[..n].copy_from_slice(v.get(..n)?);
            let u = u64::from_le_bytes(raw);
            Some(if *signed {
                let shift = 64 - 8 * n as u32;
                ((u << shift) as i64 >> shift) as f64
            } else {
                u as f64
            })
        }
        _ => None,
    }
}

fn db(a: f64) -> f64 {
    if a <= 0.0 { -96.0 } else { 20.0 * a.log10() }
}

impl<'a> Reader<'a> {
    fn new(root: &'a Object, rate: u32) -> Reader<'a> {
        let mut r = Reader {
            rate: rate.max(1),
            mobs: HashMap::new(),
            essence: HashMap::new(),
            parents: HashMap::new(),
            pids: HashMap::new(),
            operations: HashMap::new(),
            media: Vec::new(),
            media_of: HashMap::new(),
            notes: BTreeSet::new(),
        };
        if let Some(meta) = root.strong(pid::ROOT_META_DICTIONARY) {
            for c in meta.objects(pid::META_CLASS_DEFINITIONS) {
                let Some(id) = c.auid(pid::META_IDENTIFICATION) else {
                    continue;
                };
                if let Some(parent) = c.weak_auid(pid::CLASS_PARENT) {
                    r.parents.insert(id, parent);
                }
                for p in c.objects(pid::CLASS_PROPERTIES) {
                    if let (Some(pid), Some(pid_local)) = (
                        p.auid(pid::META_IDENTIFICATION),
                        p.u16(pid::PROPERTY_LOCAL_ID),
                    ) {
                        r.pids.insert(pid, pid_local);
                    }
                }
            }
        }
        let header = root.strong(pid::ROOT_HEADER);
        if let Some(content) = header.and_then(|h| h.strong(pid::HEADER_CONTENT)) {
            for m in content.objects(pid::CONTENT_MOBS) {
                if let Some(id) = m.mob_id(pid::MOB_MOB_ID) {
                    r.mobs.insert(id, m);
                }
            }
            for e in content.objects(pid::CONTENT_ESSENCE_DATA) {
                if let (Some(id), Some(Stream::InFile { entry, len })) = (
                    e.mob_id(pid::ESSENCE_DATA_MOB_ID),
                    e.stream(pid::ESSENCE_DATA_DATA),
                ) {
                    r.essence.insert(id, (*entry, *len));
                }
            }
        }
        if let Some(dict) = header.and_then(|h| h.strong(pid::HEADER_DICTIONARY)) {
            for o in dict.objects(pid::DICTIONARY_OPERATION_DEFINITIONS) {
                if let (Some(id), Some(name)) = (
                    o.auid(pid::DEFINITION_IDENTIFICATION),
                    o.string(pid::DEFINITION_NAME),
                ) {
                    r.operations.insert(id, name);
                }
            }
        }
        r
    }

    fn is_a(&self, class: Auid, ancestor: Auid) -> bool {
        let mut c = class;
        for _ in 0..64 {
            if c == ancestor {
                return true;
            }
            if let Some(d) = model::class(c) {
                if d.parent == d.id {
                    return false;
                }
                c = d.parent;
            } else if let Some(&p) = self.parents.get(&c) {
                if p == c {
                    return false;
                }
                c = p;
            } else {
                return false;
            }
        }
        false
    }

    /// The local id of an extension property in this file.
    fn ext(&self, class: Auid, name: &str) -> Option<u16> {
        let p = model::property(class, name)?;
        Some(*self.pids.get(&p.id).unwrap_or(&p.pid))
    }

    fn data_kind(o: &Object) -> Option<Auid> {
        o.weak_auid(pid::COMPONENT_DATA_DEFINITION)
    }

    /// The composition to read: the top-level one, else one no other
    /// composition uses; the one with the most sound slots.
    fn top_level(&self) -> Option<&'a Object> {
        let comps: Vec<&Object> = self
            .mobs
            .values()
            .copied()
            .filter(|m| self.is_a(m.class, cls::COMPOSITION_MOB))
            .collect();
        let sound_slots = |m: &Object| {
            m.objects(pid::MOB_SLOTS)
                .iter()
                .filter(|s| {
                    s.strong(pid::SLOT_SEGMENT)
                        .and_then(Self::data_kind)
                        .is_some_and(defs::is_sound)
                })
                .count()
        };
        let top: Vec<&Object> = comps
            .iter()
            .copied()
            .filter(|m| m.auid(pid::MOB_USAGE_CODE) == Some(defs::USAGE_TOP_LEVEL))
            .collect();
        let pool = if top.is_empty() {
            let mut used = BTreeSet::new();
            for m in &comps {
                collect_sources(m, &mut used);
            }
            comps
                .iter()
                .copied()
                .filter(|m| {
                    m.mob_id(pid::MOB_MOB_ID)
                        .is_none_or(|id| !used.contains(&id))
                })
                .collect::<Vec<_>>()
        } else {
            top
        };
        let mut pool = pool;
        // Steady choice: by name, then id.
        pool.sort_by_key(|m| (m.string(pid::MOB_NAME), m.mob_id(pid::MOB_MOB_ID)));
        pool.into_iter().max_by_key(|m| sound_slots(m))
    }

    fn composition(&mut self, mob: &'a Object) -> Composition {
        let mut comp = Composition {
            name: mob.string(pid::MOB_NAME).unwrap_or_default(),
            rate: self.rate,
            ..Composition::default()
        };
        let mut number = 0;
        for slot in mob.objects(pid::MOB_SLOTS) {
            let Some(seg) = slot.strong(pid::SLOT_SEGMENT) else {
                continue;
            };
            let er = slot
                .rational(pid::TIMELINE_SLOT_EDIT_RATE)
                .or_else(|| slot.rational(pid::EVENT_SLOT_EDIT_RATE))
                .unwrap_or(Rational::new(self.rate as i32, 1));
            let origin = slot.i64(pid::TIMELINE_SLOT_ORIGIN).unwrap_or(0);
            let kind = Self::data_kind(seg);
            if kind.is_some_and(defs::is_timecode) {
                if comp.timecode.is_none() {
                    comp.timecode = self.timecode(seg, er);
                }
            } else if kind.is_some_and(defs::is_sound) {
                number += 1;
                let mut pieces = Vec::new();
                self.segment(seg, -origin, er, &mut pieces, 0);
                let name = slot
                    .string(pid::SLOT_NAME)
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| format!("Audio {number}"));
                comp.tracks.push(Track {
                    name,
                    clips: pieces.into_iter().map(|p| p.clip).collect(),
                });
            } else if kind == Some(defs::DATA_DESCRIPTIVE_METADATA)
                || self.is_a(slot.class, cls::EVENT_MOB_SLOT)
            {
                self.markers(seg, -origin, er, &mut comp.markers);
            } else if kind
                .is_some_and(|k| k == defs::DATA_PICTURE || k == defs::DATA_LEGACY_PICTURE)
            {
                self.notes.insert("picture tracks were left out".into());
            }
        }
        comp.markers.sort_by_key(|m| m.position);
        comp.media = std::mem::take(&mut self.media);
        comp
    }

    fn timecode(&self, seg: &Object, er: Rational) -> Option<Timecode> {
        let tc = if self.is_a(seg.class, cls::TIMECODE) {
            seg
        } else {
            seg.objects(pid::SEQUENCE_COMPONENTS)
                .iter()
                .find(|c| self.is_a(c.class, cls::TIMECODE))?
        };
        Some(Timecode {
            start: tc.i64(pid::TIMECODE_START)?,
            fps: tc.u16(pid::TIMECODE_FPS)?,
            drop: tc.bool(pid::TIMECODE_DROP).unwrap_or(false),
            rate: (er.num, er.den),
        })
    }

    fn markers(&self, seg: &Object, start: i64, er: Rational, out: &mut Vec<Marker>) {
        let list: Vec<&Object> = if self.is_a(seg.class, cls::SEQUENCE) {
            seg.objects(pid::SEQUENCE_COMPONENTS).iter().collect()
        } else {
            vec![seg]
        };
        let color_pid = self.ext(cls::COMMENT_MARKER, "CommentMarkerColor");
        for e in list {
            if !self.is_a(e.class, cls::EVENT) {
                continue;
            }
            let Some(at) = e.i64(pid::EVENT_POSITION) else {
                continue;
            };
            let color = color_pid.and_then(|p| e.data(p)).and_then(|b| {
                let c = |k: usize| -> Option<u8> {
                    Some((u16::from_le_bytes(b.get(2 * k..2 * k + 2)?.try_into().ok()?) >> 8) as u8)
                };
                Some([c(0)?, c(1)?, c(2)?])
            });
            out.push(Marker {
                position: rescale(at + start, er, self.rate),
                name: e.string(pid::EVENT_COMMENT).unwrap_or_default(),
                color,
            });
        }
    }

    /// Flatten `seg` placed at `start` (edit units of `er`) into pieces;
    /// its length.
    fn segment(
        &mut self,
        seg: &'a Object,
        start: i64,
        er: Rational,
        out: &mut Vec<Piece>,
        depth: usize,
    ) -> i64 {
        let length = seg.i64(pid::COMPONENT_LENGTH).unwrap_or(0);
        if depth > 64 {
            self.notes
                .insert("structures nested too deep were left out".into());
            return length;
        }
        let class = seg.class;
        if self.is_a(class, cls::SEQUENCE) {
            return self.sequence(seg, start, er, out, depth);
        }
        if self.is_a(class, cls::SOURCE_CLIP) {
            self.source_clip(seg, start, er, out, depth);
            return length;
        }
        if self.is_a(class, cls::FILLER) {
            return length;
        }
        if self.is_a(class, cls::OPERATION_GROUP) {
            return self.operation(seg, start, er, out, depth);
        }
        if self.is_a(class, cls::SELECTOR) {
            if let Some(s) = seg.strong(pid::SELECTOR_SELECTED) {
                self.segment(s, start, er, out, depth + 1);
            }
            return length;
        }
        if self.is_a(class, cls::ESSENCE_GROUP) {
            if let Some(s) = seg.objects(pid::ESSENCE_GROUP_CHOICES).first() {
                self.segment(s, start, er, out, depth + 1);
            }
            return length;
        }
        if self.is_a(class, cls::NESTED_SCOPE) {
            if let Some(s) = seg.objects(pid::NESTED_SCOPE_SLOTS).last() {
                self.segment(s, start, er, out, depth + 1);
            }
            return length;
        }
        if self.is_a(class, cls::SCOPE_REFERENCE) {
            self.notes
                .insert("scope references (shared material) were left out".into());
            return length;
        }
        if !self.is_a(class, cls::TIMECODE) && !self.is_a(class, cls::EVENT) {
            let name = model::class(class).map_or_else(|| class.to_string(), |c| c.name.into());
            self.notes.insert(format!("{name} segments were left out"));
        }
        length
    }

    fn sequence(
        &mut self,
        seq: &'a Object,
        start: i64,
        er: Rational,
        out: &mut Vec<Piece>,
        depth: usize,
    ) -> i64 {
        let mut pos = start;
        // The pieces each segment made, and the transitions after them.
        let mut ranges: Vec<std::ops::Range<usize>> = Vec::new();
        let mut transitions: Vec<Between> = Vec::new();
        for c in seq.objects(pid::SEQUENCE_COMPONENTS) {
            if self.is_a(c.class, cls::TRANSITION) {
                let len = c.i64(pid::COMPONENT_LENGTH).unwrap_or(0).max(0);
                pos -= len;
                let level = c
                    .strong(pid::TRANSITION_OPERATION_GROUP)
                    .map(|g| self.level_points(g))
                    .unwrap_or_default();
                transitions.push((ranges.len(), len, level));
                continue;
            }
            let before = out.len();
            let len = self.segment(c, pos, er, out, depth + 1);
            ranges.push(before..out.len());
            pos += len;
        }
        for (next, len, level) in transitions {
            let samples = rescale(len, er, self.rate);
            let prev = next.checked_sub(1).and_then(|k| ranges.get(k)).cloned();
            let after = ranges.get(next).cloned();
            let has =
                |r: &Option<std::ops::Range<usize>>| r.as_ref().is_some_and(|r| !r.is_empty());
            let fade = |curve| Fade {
                length: samples,
                curve,
            };
            match (has(&prev), has(&after)) {
                (true, true) => {
                    let curve = if level.is_empty() {
                        FadeCurve::Linear
                    } else {
                        FadeCurve::nearest(&level)
                    };
                    if let Some(p) = last_of(out, prev) {
                        out[p].clip.fade_out = Some(fade(curve));
                    }
                    if let Some(n) = first_of(out, after) {
                        out[n].clip.fade_in = Some(fade(curve));
                    }
                }
                (false, true) => {
                    let curve = if level.is_empty() {
                        FadeCurve::Linear
                    } else {
                        FadeCurve::nearest(&level)
                    };
                    if let Some(n) = first_of(out, after) {
                        out[n].clip.fade_in = Some(fade(curve));
                    }
                }
                (true, false) => {
                    let curve = if level.is_empty() {
                        FadeCurve::Linear
                    } else {
                        let flipped: Vec<(f64, f64)> =
                            level.iter().map(|&(t, l)| (1.0 - t, 1.0 - l)).collect();
                        FadeCurve::nearest(&flipped)
                    };
                    if let Some(p) = last_of(out, prev) {
                        out[p].clip.fade_out = Some(fade(curve));
                    }
                }
                (false, false) => {}
            }
        }
        pos - start
    }

    /// A dissolve's Level as (time fraction, level) points.
    fn level_points(&self, group: &Object) -> Vec<(f64, f64)> {
        let params = group.objects(pid::OPERATION_GROUP_PARAMETERS);
        let Some(v) = params.iter().find(|p| {
            p.auid(pid::PARAMETER_DEFINITION) == Some(defs::PARAMETER_LEVEL)
                && self.is_a(p.class, cls::VARYING_VALUE)
        }) else {
            return Vec::new();
        };
        v.objects(pid::VARYING_VALUE_POINT_LIST)
            .iter()
            .filter_map(|p| {
                Some((
                    p.rational(pid::CONTROL_POINT_TIME)?.to_f64(),
                    indirect_number(p.data(pid::CONTROL_POINT_VALUE)?)?,
                ))
            })
            .collect()
    }

    fn operation(
        &mut self,
        group: &'a Object,
        start: i64,
        er: Rational,
        out: &mut Vec<Piece>,
        depth: usize,
    ) -> i64 {
        let length = group.i64(pid::COMPONENT_LENGTH).unwrap_or(0);
        let op = group.weak_auid(pid::OPERATION_GROUP_OPERATION);
        let before = out.len();
        let inputs = group.objects(pid::OPERATION_GROUP_INPUT_SEGMENTS);
        if let Some(first) = inputs.first() {
            self.segment(first, start, er, out, depth + 1);
        }
        let params = group.objects(pid::OPERATION_GROUP_PARAMETERS);
        let amplitude = params
            .iter()
            .find(|p| p.auid(pid::PARAMETER_DEFINITION) == Some(defs::PARAMETER_AMPLITUDE));
        let is_gain = op.is_some_and(|o| {
            o == defs::OPERATION_MONO_AUDIO_GAIN || o == defs::OPERATION_STEREO_AUDIO_GAIN
        });
        match (amplitude, is_gain) {
            (Some(p), _) => {
                let place = rescale(start, er, self.rate);
                let span = rescale(length, er, self.rate);
                if self.is_a(p.class, cls::CONSTANT_VALUE) {
                    if let Some(a) = p.data(pid::CONSTANT_VALUE_VALUE).and_then(indirect_number) {
                        for piece in &mut out[before..] {
                            piece.clip.gain_db += db(a);
                        }
                    }
                } else {
                    let points: Vec<(i64, f64)> = p
                        .objects(pid::VARYING_VALUE_POINT_LIST)
                        .iter()
                        .filter_map(|cp| {
                            let t = cp.rational(pid::CONTROL_POINT_TIME)?.to_f64();
                            let a = indirect_number(cp.data(pid::CONTROL_POINT_VALUE)?)?;
                            Some((place + (t * span as f64).round() as i64, a))
                        })
                        .collect();
                    for piece in &mut out[before..] {
                        let at = piece.clip.position;
                        let own: Vec<(i64, f64)> =
                            points.iter().map(|&(x, a)| (x - at, db(a))).collect();
                        piece.clip.envelope = combine(&piece.clip.envelope, &own);
                    }
                }
            }
            (None, true) => {}
            (None, false) => {
                let name = op
                    .and_then(|o| self.operations.get(&o).cloned())
                    .unwrap_or_else(|| "an effect".into());
                self.notes.insert(format!("{name} was left out"));
            }
        }
        length
    }

    fn source_clip(
        &mut self,
        sc: &'a Object,
        start: i64,
        er: Rational,
        out: &mut Vec<Piece>,
        depth: usize,
    ) {
        let length = sc.i64(pid::COMPONENT_LENGTH).unwrap_or(0);
        if length <= 0 || !Self::data_kind(sc).is_none_or(defs::is_sound) {
            return;
        }
        let Some(source) = sc.mob_id(pid::SOURCE_ID).filter(|m| !m.is_nil()) else {
            return;
        };
        let slot = sc.u32(pid::SOURCE_MOB_SLOT_ID).unwrap_or(1);
        let from = sc.i64(pid::SOURCE_CLIP_START_TIME).unwrap_or(0);
        let channel_ids: Vec<u32> = sc
            .data(pid::SOURCE_CHANNEL_IDS)
            .map(|b| {
                b.as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| u32::from_le_bytes(*c))
                    .collect()
            })
            .unwrap_or_default();
        let position = rescale(start, er, self.rate);
        let samples = rescale(start + length, er, self.rate) - position;
        let fade = |len: Option<i64>, kind: Option<u8>| {
            let len = rescale(len?, er, self.rate);
            (len > 0).then_some(Fade {
                length: len,
                curve: if kind == Some(2) {
                    FadeCurve::EqualPower
                } else {
                    FadeCurve::Linear
                },
            })
        };
        let fade_in = fade(
            sc.i64(pid::SOURCE_CLIP_FADE_IN_LENGTH),
            sc.u8(pid::SOURCE_CLIP_FADE_IN_TYPE),
        );
        let fade_out = fade(
            sc.i64(pid::SOURCE_CLIP_FADE_OUT_LENGTH),
            sc.u8(pid::SOURCE_CLIP_FADE_OUT_TYPE),
        );
        let Some(found) = self.resolve(source, slot, from, er, depth) else {
            return;
        };
        match found {
            Found::Essence {
                media,
                channel,
                offset,
                name,
            } => {
                let channel = channel_ids
                    .first()
                    .map(|&c| c.saturating_sub(1) as u16)
                    .unwrap_or(channel);
                out.push(Piece {
                    clip: Clip {
                        name,
                        position,
                        length: samples,
                        media,
                        channel: channel.min(self.media[media].channels.saturating_sub(1)),
                        offset,
                        gain_db: 0.0,
                        envelope: Vec::new(),
                        fade_in,
                        fade_out,
                    },
                });
            }
            Found::Composition {
                mob,
                slot,
                at,
                er: inner,
            } => {
                // A nested composition: its pieces inside the window.
                let mut inner_pieces = Vec::new();
                if let Some(seg) = mob
                    .objects(pid::MOB_SLOTS)
                    .iter()
                    .find(|s| s.u32(pid::SLOT_ID) == Some(slot))
                    .and_then(|s| s.strong(pid::SLOT_SEGMENT))
                {
                    self.segment(seg, 0, inner, &mut inner_pieces, depth + 1);
                }
                let shift = position - rescale(at, inner, self.rate);
                let (a, b) = (position, position + samples);
                for mut p in inner_pieces {
                    p.clip.position += shift;
                    let (s, e) = (p.clip.position.max(a), p.clip.end().min(b));
                    if e <= s {
                        continue;
                    }
                    let cut = s - p.clip.position;
                    p.clip.offset += cut;
                    p.clip.envelope = p.clip.envelope.iter().map(|&(x, v)| (x - cut, v)).collect();
                    if cut > 0 {
                        p.clip.fade_in = None;
                    }
                    if p.clip.end() > e {
                        p.clip.fade_out = None;
                    }
                    p.clip.position = s;
                    p.clip.length = e - s;
                    out.push(p);
                }
            }
        }
    }

    /// Follow `mob`'s `slot` at `from` (units of `er`) to the sound.
    fn resolve(
        &mut self,
        mob_id: MobId,
        slot: u32,
        from: i64,
        er: Rational,
        depth: usize,
    ) -> Option<Found<'a>> {
        if depth > 64 {
            return None;
        }
        let Some(&mob) = self.mobs.get(&mob_id) else {
            self.notes
                .insert("clips whose media is not in the file were left out".into());
            return None;
        };
        let slots = mob.objects(pid::MOB_SLOTS);
        let s = slots
            .iter()
            .find(|s| s.u32(pid::SLOT_ID) == Some(slot))
            .or_else(|| (slots.len() == 1).then(|| &slots[0]))?;
        let slot_er = s.rational(pid::TIMELINE_SLOT_EDIT_RATE).unwrap_or(er);
        let at = convert(from, er, slot_er) + s.i64(pid::TIMELINE_SLOT_ORIGIN).unwrap_or(0);
        if self.is_a(mob.class, cls::COMPOSITION_MOB) {
            return Some(Found::Composition {
                mob,
                slot,
                at,
                er: slot_er,
            });
        }
        if self.is_a(mob.class, cls::SOURCE_MOB)
            && let Some(d) = mob.strong(pid::SOURCE_MOB_ESSENCE_DESCRIPTION)
            && self.is_a(d.class, cls::FILE_DESCRIPTOR)
        {
            let (media, channel) = self.media_for(mob_id, mob, d, slot);
            let rate = self.media[media].rate;
            let offset = rescale(at, slot_er, rate);
            // Offsets are told at the composition's rate.
            let offset =
                (i128::from(offset) * i128::from(self.rate) / i128::from(rate.max(1))) as i64;
            let name = self.media[media].name.clone();
            return Some(Found::Essence {
                media,
                channel,
                offset,
                name,
            });
        }
        // A master mob (or a source mob between): the clip in its slot at
        // that time.
        let seg = s.strong(pid::SLOT_SEGMENT)?;
        let (clip, within) = self.clip_at(seg, at)?;
        let next = clip.mob_id(pid::SOURCE_ID).filter(|m| !m.is_nil())?;
        let next_slot = clip.u32(pid::SOURCE_MOB_SLOT_ID).unwrap_or(1);
        let next_from = clip.i64(pid::SOURCE_CLIP_START_TIME).unwrap_or(0) + within;
        let mut found = self.resolve(next, next_slot, next_from, slot_er, depth + 1)?;
        // Media are named as their master clip is.
        if let Found::Essence { name, media, .. } = &mut found
            && let Some(n) = mob.string(pid::MOB_NAME).filter(|n| !n.trim().is_empty())
            && self.is_a(mob.class, cls::MASTER_MOB)
        {
            self.media[*media].name = n.clone();
            *name = n;
        }
        Some(found)
    }

    /// The source clip of `seg` playing at `at`, and how far into it.
    fn clip_at(&self, seg: &'a Object, at: i64) -> Option<(&'a Object, i64)> {
        if self.is_a(seg.class, cls::SOURCE_CLIP) {
            return Some((seg, at));
        }
        if self.is_a(seg.class, cls::ESSENCE_GROUP) {
            return self.clip_at(seg.objects(pid::ESSENCE_GROUP_CHOICES).first()?, at);
        }
        if self.is_a(seg.class, cls::SELECTOR) {
            return self.clip_at(seg.strong(pid::SELECTOR_SELECTED)?, at);
        }
        if self.is_a(seg.class, cls::OPERATION_GROUP) {
            return self.clip_at(
                seg.objects(pid::OPERATION_GROUP_INPUT_SEGMENTS).first()?,
                at,
            );
        }
        if self.is_a(seg.class, cls::SEQUENCE) {
            let mut pos = 0;
            for c in seg.objects(pid::SEQUENCE_COMPONENTS) {
                let len = c.i64(pid::COMPONENT_LENGTH).unwrap_or(0);
                if self.is_a(c.class, cls::TRANSITION) {
                    pos -= len;
                    continue;
                }
                if at >= pos && at < pos + len {
                    return self.clip_at(c, at - pos);
                }
                pos += len;
            }
        }
        None
    }

    /// The medium a file source mob is (made once).
    fn media_for(&mut self, id: MobId, mob: &Object, d: &Object, slot: u32) -> (usize, u16) {
        // A multiple descriptor: the descriptor of the slot.
        let d = if self.is_a(d.class, cls::MULTIPLE_DESCRIPTOR) {
            let list = d.objects(pid::MULTIPLE_FILE_DESCRIPTORS);
            list.iter()
                .find(|f| f.u32(pid::FILE_DESCRIPTOR_LINKED_SLOT_ID) == Some(slot))
                .or_else(|| list.get(slot.saturating_sub(1) as usize))
                .or_else(|| list.first())
                .unwrap_or(d)
        } else {
            d
        };
        if let Some(&m) = self.media_of.get(&id) {
            return (m, 0);
        }
        let rate = d
            .rational(pid::SOUND_AUDIO_SAMPLING_RATE)
            .or_else(|| d.rational(pid::FILE_DESCRIPTOR_SAMPLE_RATE))
            .map_or(self.rate, |r| r.to_f64().round().max(1.0) as u32);
        let sample_rate = d
            .rational(pid::FILE_DESCRIPTOR_SAMPLE_RATE)
            .unwrap_or(Rational::new(rate as i32, 1));
        let frames = d
            .i64(pid::FILE_DESCRIPTOR_LENGTH)
            .map_or(0, |l| rescale(l, sample_rate, rate));
        let mut channels = d.u32(pid::SOUND_CHANNELS).unwrap_or(1).clamp(1, 64) as u16;
        let mut format = EssenceFormat::Pcm {
            bits: d.u32(pid::SOUND_QUANTIZATION_BITS).unwrap_or(16) as u16,
            channels,
            rate,
            big_endian: false,
            float: false,
        };
        if self.is_a(d.class, cls::WAVE_DESCRIPTOR) {
            if let Some(f) = d.data(pid::WAVE_SUMMARY).and_then(crate::wave::wave_format) {
                channels = f.channels;
                format = f.pcm(false);
            }
            format = match format {
                EssenceFormat::Pcm { .. } if d.data(pid::WAVE_SUMMARY).is_none() => {
                    EssenceFormat::Wave
                }
                f => f,
            };
        } else if self.is_a(d.class, cls::AIFC_DESCRIPTOR)
            && let Some(f) = d.data(pid::AIFC_SUMMARY).and_then(crate::wave::aiff_format)
        {
            channels = f.channels;
            format = f.pcm(true);
        }
        let urls: Vec<String> = d
            .objects(pid::ESSENCE_DESCRIPTOR_LOCATOR)
            .iter()
            .filter_map(|l| l.data(pid::NETWORK_LOCATOR_URL).map(decode_string))
            .filter(|u| !u.trim().is_empty())
            .collect();
        let name = mob
            .string(pid::MOB_NAME)
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| "Audio".into());
        let essence: Vec<Essence> = (0..channels)
            .map(|c| {
                if let Some(&(entry, len)) = self.essence.get(&id) {
                    Essence::Embedded(Embedded {
                        entry,
                        len,
                        format,
                        channel: c,
                    })
                } else if !urls.is_empty() {
                    let format = if self.is_a(d.class, cls::AIFC_DESCRIPTOR) {
                        EssenceFormat::Aiff
                    } else {
                        EssenceFormat::Wave
                    };
                    Essence::Linked {
                        urls: urls.clone(),
                        channel: c,
                        format,
                    }
                } else {
                    Essence::Missing
                }
            })
            .collect();
        if essence.iter().any(|e| matches!(e, Essence::Missing)) {
            self.notes
                .insert("some clips' media is neither in the file nor linked (offline)".into());
        }
        self.media.push(Media {
            name,
            channels,
            rate,
            frames,
            essence,
        });
        let m = self.media.len() - 1;
        self.media_of.insert(id, m);
        (m, 0)
    }
}

enum Found<'a> {
    Essence {
        media: usize,
        channel: u16,
        offset: i64,
        name: String,
    },
    Composition {
        mob: &'a Object,
        slot: u32,
        at: i64,
        er: Rational,
    },
}

fn first_of(out: &[Piece], r: Option<std::ops::Range<usize>>) -> Option<usize> {
    r?.min_by_key(|&i| out[i].clip.position)
}

fn last_of(out: &[Piece], r: Option<std::ops::Range<usize>>) -> Option<usize> {
    r?.max_by_key(|&i| out[i].clip.end())
}

/// Every source a mob's slots name.
fn collect_sources(o: &Object, into: &mut BTreeSet<MobId>) {
    if let Some(id) = o.mob_id(pid::SOURCE_ID) {
        into.insert(id);
    }
    for p in &o.props {
        match &p.value {
            Value::Strong(c) => collect_sources(c, into),
            Value::StrongVector(v) | Value::StrongSet { items: v, .. } => {
                for c in v {
                    collect_sources(c, into);
                }
            }
            _ => {}
        }
    }
}

/// Two level envelopes (points in dB, straight in amplitude) multiplied.
fn combine(a: &[(i64, f64)], b: &[(i64, f64)]) -> Vec<(i64, f64)> {
    if a.is_empty() {
        return b.to_vec();
    }
    if b.is_empty() {
        return a.to_vec();
    }
    let at = |env: &[(i64, f64)], x: i64| -> f64 {
        let i = env.partition_point(|p| p.0 <= x);
        let amp = |d: f64| 10f64.powf(d / 20.0);
        match (i.checked_sub(1).map(|k| env[k]), env.get(i)) {
            (Some(p), Some(n)) if n.0 > p.0 => {
                let t = (x - p.0) as f64 / (n.0 - p.0) as f64;
                amp(p.1) + (amp(n.1) - amp(p.1)) * t
            }
            (Some(p), _) => amp(p.1),
            (None, Some(n)) => amp(n.1),
            (None, None) => 1.0,
        }
    };
    let mut xs: Vec<i64> = a.iter().chain(b).map(|p| p.0).collect();
    xs.sort_unstable();
    xs.dedup();
    xs.into_iter()
        .map(|x| (x, db(at(a, x) * at(b, x))))
        .collect()
}

#[cfg(test)]
mod tests;
