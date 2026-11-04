//! Writing a [`Composition`] as an AAF file, the way the Edit Protocol
//! lays one out and Pro Tools, Media Composer and Resolve read it:
//!
//! * a top-level CompositionMob: a timecode slot (when the composition
//!   has a timecode), a sound slot per mono track (clips that overlap
//!   other than as a crossfade go to another slot of the same name with a
//!   number), an event slot of DescriptiveMarkers;
//! * a MasterMob per medium with a slot per channel, each pointing at a
//!   mono file SourceMob whose descriptor says where the sound is:
//!   embedded (PCMDescriptor, the samples in an EssenceData stream, 16 or
//!   24 bit) or linked (WAVEDescriptor with a NetworkLocator URL);
//! * fades as Transitions (Mono Audio Dissolve) against Filler, crossfades
//!   as Transitions between the two clips, the curve in the dissolve's
//!   Level parameter; clip gain as a Mono Audio Gain OperationGroup around
//!   the clip (a ConstantValue Amplitude, or a VaryingValue when the clip
//!   has a volume envelope).
//!
//! The meta dictionary is written whole. Large essence goes straight to
//! the file as it is converted.

use crate::cfb::{CfbWriter, SectorSize};
use crate::ids::{Auid, IdSource, MobId};
use crate::model::{cls, defs, pid, ty};
use crate::store::write::{StoreError, StoreWriter};
use crate::store::{Object, Rational, Stream, TimeStamp, Value};
use crate::timeline::{Clip, Composition, Essence, Fade, FadeCurve};
use std::io::{self, Seek, Write};

/// Where embedded essence comes from: `out.len()` frames of `channel` of
/// medium `media` from frame `at`, as floats.
pub trait Samples {
    fn read(&mut self, media: usize, channel: u16, at: u64, out: &mut [f32]) -> io::Result<()>;
}

#[derive(Clone, Debug)]
pub struct WriteOptions {
    pub sector_size: SectorSize,
    pub ids: IdSource,
    /// The writing program's version ("0.36.0").
    pub version: String,
    /// When the file was made.
    pub when: TimeStamp,
    /// The platform it was made on (Identification).
    pub platform: String,
}

impl Default for WriteOptions {
    fn default() -> Self {
        WriteOptions {
            sector_size: SectorSize::Large,
            ids: IdSource::random(),
            version: String::new(),
            when: TimeStamp::now(),
            platform: std::env::consts::OS.into(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Invalid(String),
}

/// What was written.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Written {
    /// Sound slots of the composition (tracks and their extra lanes).
    pub slots: usize,
    pub clips: usize,
    pub transitions: usize,
    /// Bytes of embedded essence.
    pub embedded: u64,
}

fn data_def(d: Auid) -> Value {
    Value::weak(ty::DATA_DEFINITION_WEAK, d)
}

fn edit_rate(rate: u32) -> Value {
    Value::rational(Rational::new(rate as i32, 1))
}

/// A position inside an operation as a fraction of its length.
fn fraction(at: i64, length: i64) -> Rational {
    if length > 0 && length <= i64::from(i32::MAX) && (0..=length).contains(&at) {
        Rational::new(at as i32, length as i32)
    } else {
        Rational::of_f64(at as f64 / length.max(1) as f64)
    }
}

fn definition(class: Auid, id: Auid, name: &str, description: &str) -> Object {
    let mut o = Object::new(class)
        .with(pid::DEFINITION_IDENTIFICATION, Value::auid(id))
        .with(pid::DEFINITION_NAME, Value::string(name));
    if !description.is_empty() {
        o.set(pid::DEFINITION_DESCRIPTION, Value::string(description));
    }
    o
}

fn definitions(items: Vec<Object>) -> Value {
    Value::StrongSet {
        key_pid: pid::DEFINITION_IDENTIFICATION,
        key_from: pid::DEFINITION_IDENTIFICATION,
        key_size: 16,
        items,
    }
}

fn dictionary() -> Object {
    let data = defs::DATA_DEFINITIONS
        .iter()
        .map(|(id, name, d)| definition(cls::DATA_DEFINITION, *id, name, d))
        .collect();
    let containers = defs::CONTAINER_DEFINITIONS
        .iter()
        .map(|(id, name)| definition(cls::CONTAINER_DEFINITION, *id, name, ""))
        .collect();
    let interpolations = defs::INTERPOLATION_DEFINITIONS
        .iter()
        .map(|(id, name)| definition(cls::INTERPOLATION_DEFINITION, *id, name, ""))
        .collect();
    let parameter = |id, name, units| {
        definition(cls::PARAMETER_DEFINITION, id, name, "")
            .with(
                pid::PARAMETER_DEF_TYPE,
                Value::weak(ty::TYPE_DEFINITION_WEAK, ty::RATIONAL),
            )
            .with(pid::PARAMETER_DEF_DISPLAY_UNITS, Value::string(units))
    };
    let parameters = vec![
        parameter(defs::PARAMETER_AMPLITUDE, "Amplitude", "factor"),
        parameter(defs::PARAMETER_LEVEL, "Level", "factor"),
    ];
    let operation = |id, name, inputs: i32, param: Auid| {
        definition(cls::OPERATION_DEFINITION, id, name, "")
            .with(
                pid::OPERATION_DEF_DATA_DEFINITION,
                data_def(defs::DATA_SOUND),
            )
            .with(pid::OPERATION_DEF_IS_TIME_WARP, Value::bool(false))
            .with(
                pid::OPERATION_DEF_CATEGORY,
                Value::auid(defs::OPERATION_CATEGORY_EFFECT),
            )
            .with(pid::OPERATION_DEF_NUMBER_INPUTS, Value::i32(inputs))
            .with(pid::OPERATION_DEF_BYPASS, Value::u32(1))
            .with(
                pid::OPERATION_DEF_PARAMETERS_DEFINED,
                Value::WeakSet(Value::weak_refs(
                    ty::PARAMETER_DEFINITION_WEAK_SET,
                    &[param],
                )),
            )
    };
    let operations = vec![
        operation(
            defs::OPERATION_MONO_AUDIO_GAIN,
            "Mono Audio Gain",
            1,
            defs::PARAMETER_AMPLITUDE,
        ),
        operation(
            defs::OPERATION_MONO_AUDIO_DISSOLVE,
            "Mono Audio Dissolve",
            2,
            defs::PARAMETER_LEVEL,
        ),
    ];
    Object::new(cls::DICTIONARY)
        .with(
            pid::DICTIONARY_OPERATION_DEFINITIONS,
            definitions(operations),
        )
        .with(
            pid::DICTIONARY_PARAMETER_DEFINITIONS,
            definitions(parameters),
        )
        .with(pid::DICTIONARY_DATA_DEFINITIONS, definitions(data))
        .with(
            pid::DICTIONARY_CONTAINER_DEFINITIONS,
            definitions(containers),
        )
        .with(
            pid::DICTIONARY_INTERPOLATION_DEFINITIONS,
            definitions(interpolations),
        )
}

fn identification(options: &WriteOptions, generation: Auid) -> Object {
    let mut v = options
        .version
        .split('.')
        .map(|p| p.trim().parse::<u16>().unwrap_or(0));
    let mut version = Vec::with_capacity(9);
    for _ in 0..4 {
        version.extend_from_slice(&v.next().unwrap_or(0).to_le_bytes());
    }
    version.push(1); // VersionReleased
    Object::new(cls::IDENTIFICATION)
        .with(pid::IDENT_COMPANY_NAME, Value::string("FaderFrame"))
        .with(pid::IDENT_PRODUCT_NAME, Value::string("FaderFrame"))
        .with(pid::IDENT_PRODUCT_VERSION, Value::Data(version))
        .with(
            pid::IDENT_PRODUCT_VERSION_STRING,
            Value::string(&options.version),
        )
        .with(pid::IDENT_PRODUCT_ID, Value::auid(defs::PRODUCT_ID))
        .with(pid::IDENT_DATE, Value::timestamp(options.when))
        .with(pid::IDENT_PLATFORM, Value::string(&options.platform))
        .with(pid::IDENT_GENERATION_AUID, Value::auid(generation))
}

fn mob(class: Auid, id: MobId, name: &str, slots: Vec<Object>, when: TimeStamp) -> Object {
    Object::new(class)
        .with(pid::MOB_MOB_ID, Value::mob_id(id))
        .with(pid::MOB_NAME, Value::string(name))
        .with(pid::MOB_SLOTS, Value::StrongVector(slots))
        .with(pid::MOB_LAST_MODIFIED, Value::timestamp(when))
        .with(pid::MOB_CREATION_TIME, Value::timestamp(when))
}

fn timeline_slot(slot: u32, name: &str, number: u32, rate: Value, segment: Object) -> Object {
    let mut o = Object::new(cls::TIMELINE_MOB_SLOT).with(pid::SLOT_ID, Value::u32(slot));
    if !name.is_empty() {
        o.set(pid::SLOT_NAME, Value::string(name));
    }
    o.with(pid::SLOT_PHYSICAL_TRACK_NUMBER, Value::u32(number))
        .with(pid::TIMELINE_SLOT_EDIT_RATE, rate)
        .with(pid::TIMELINE_SLOT_ORIGIN, Value::i64(0))
        .with(pid::SLOT_SEGMENT, Value::Strong(Box::new(segment)))
}

fn component(class: Auid, data: Auid, length: i64) -> Object {
    Object::new(class)
        .with(pid::COMPONENT_DATA_DEFINITION, data_def(data))
        .with(pid::COMPONENT_LENGTH, Value::i64(length))
}

fn source_clip(length: i64, start: i64, source: MobId, slot: u32) -> Object {
    component(cls::SOURCE_CLIP, defs::DATA_SOUND, length)
        .with(pid::SOURCE_CLIP_START_TIME, Value::i64(start))
        .with(pid::SOURCE_ID, Value::mob_id(source))
        .with(pid::SOURCE_MOB_SLOT_ID, Value::u32(slot))
}

fn control_point(time: Rational, value: f64) -> Object {
    Object::new(cls::CONTROL_POINT)
        .with(
            pid::CONTROL_POINT_VALUE,
            Value::indirect(ty::RATIONAL, &Rational::of_f64(value).encode()),
        )
        .with(pid::CONTROL_POINT_TIME, Value::rational(time))
        .with(pid::CONTROL_POINT_EDIT_HINT, Value::u8(1))
}

fn parameters(items: Vec<Object>) -> Value {
    // The AAF SDK keeps an operation's parameters in a set keyed by their
    // definition (and names DefinitionObject's Identification as the key).
    Value::StrongSet {
        key_pid: pid::DEFINITION_IDENTIFICATION,
        key_from: pid::PARAMETER_DEFINITION,
        key_size: 16,
        items,
    }
}

fn varying(definition: Auid, points: Vec<Object>) -> Object {
    Object::new(cls::VARYING_VALUE)
        .with(pid::PARAMETER_DEFINITION, Value::auid(definition))
        .with(
            pid::VARYING_VALUE_INTERPOLATION,
            Value::weak(
                ty::INTERPOLATION_DEFINITION_WEAK,
                defs::INTERPOLATION_LINEAR,
            ),
        )
        .with(pid::VARYING_VALUE_POINT_LIST, Value::StrongVector(points))
}

/// The points a dissolve's Level is drawn through for a curve: the
/// incoming side's gain.
const LEVEL_POINTS: i32 = 8;

fn transition(length: i64, cut: i64, level: impl Fn(f64) -> f64) -> Object {
    let points = (0..=LEVEL_POINTS)
        .map(|k| {
            control_point(
                Rational::new(k, LEVEL_POINTS),
                level(f64::from(k) / f64::from(LEVEL_POINTS)),
            )
        })
        .collect();
    let group = component(cls::OPERATION_GROUP, defs::DATA_SOUND, length)
        .with(
            pid::OPERATION_GROUP_OPERATION,
            Value::weak(
                ty::OPERATION_DEFINITION_WEAK,
                defs::OPERATION_MONO_AUDIO_DISSOLVE,
            ),
        )
        .with(
            pid::OPERATION_GROUP_PARAMETERS,
            parameters(vec![varying(defs::PARAMETER_LEVEL, points)]),
        );
    component(cls::TRANSITION, defs::DATA_SOUND, length)
        .with(
            pid::TRANSITION_OPERATION_GROUP,
            Value::Strong(Box::new(group)),
        )
        .with(pid::TRANSITION_CUT_POINT, Value::i64(cut))
}

fn amplitude(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// The clip's segment: the source clip, inside a gain operation when it
/// has gain or an envelope.
fn clip_segment(c: &Clip, source: MobId, slot: u32) -> Object {
    let sc = source_clip(c.length, c.offset, source, slot);
    let flat = c.envelope.is_empty();
    if flat && c.gain_db.abs() < 1e-6 {
        return sc;
    }
    let param = if flat {
        Object::new(cls::CONSTANT_VALUE)
            .with(
                pid::PARAMETER_DEFINITION,
                Value::auid(defs::PARAMETER_AMPLITUDE),
            )
            .with(
                pid::CONSTANT_VALUE_VALUE,
                Value::indirect(
                    ty::RATIONAL,
                    &Rational::of_f64(amplitude(c.gain_db)).encode(),
                ),
            )
    } else {
        let mut pts: Vec<(i64, f64)> = c
            .envelope
            .iter()
            .map(|&(at, db)| (at.clamp(0, c.length), db))
            .collect();
        pts.sort_by_key(|p| p.0);
        if let Some(&(at, db)) = pts.first()
            && at > 0
        {
            pts.insert(0, (0, db));
        }
        if let Some(&(at, db)) = pts.last()
            && at < c.length
        {
            pts.push((c.length, db));
        }
        let points = pts
            .iter()
            .map(|&(at, db)| control_point(fraction(at, c.length), amplitude(db + c.gain_db)))
            .collect();
        varying(defs::PARAMETER_AMPLITUDE, points)
    };
    component(cls::OPERATION_GROUP, defs::DATA_SOUND, c.length)
        .with(
            pid::OPERATION_GROUP_OPERATION,
            Value::weak(
                ty::OPERATION_DEFINITION_WEAK,
                defs::OPERATION_MONO_AUDIO_GAIN,
            ),
        )
        .with(
            pid::OPERATION_GROUP_INPUT_SEGMENTS,
            Value::StrongVector(vec![sc]),
        )
        .with(pid::OPERATION_GROUP_PARAMETERS, parameters(vec![param]))
}

/// A clip's fades made to fit inside it (together no longer than it).
fn fitted(c: &Clip) -> (Option<Fade>, Option<Fade>) {
    let len = |f: &Option<Fade>| f.map_or(0, |f| f.length.max(0));
    let (a, b) = (len(&c.fade_in), len(&c.fade_out));
    let scale = |f: Option<Fade>, n: i64| {
        f.filter(|f| f.length > 0).map(|f| Fade {
            length: if a + b > c.length && a + b > 0 {
                (n as i128 * c.length as i128 / (a + b) as i128) as i64
            } else {
                n
            },
            ..f
        })
    };
    (scale(c.fade_in, a), scale(c.fade_out, b))
}

/// How long `a` and `b` crossfade, when they overlap exactly by `a`'s
/// fade-out and `b`'s fade-in.
fn crossfade(a: &Clip, b: &Clip) -> Option<i64> {
    let overlap = a.end() - b.position;
    let (_, out) = fitted(a);
    let (inn, _) = fitted(b);
    (overlap > 0
        && b.position > a.position
        && b.end() > a.end()
        && out.map(|f| f.length) == Some(overlap)
        && inn.map(|f| f.length) == Some(overlap))
    .then_some(overlap)
}

/// The track's clips in lanes: each lane's clips one after another, or
/// crossfading.
pub(crate) fn lanes(clips: &[Clip]) -> Vec<Vec<usize>> {
    let mut order: Vec<usize> = (0..clips.len()).filter(|&i| clips[i].length > 0).collect();
    order.sort_by_key(|&i| (clips[i].position, clips[i].length));
    let mut lanes: Vec<Vec<usize>> = Vec::new();
    'next: for i in order {
        for lane in lanes.iter_mut() {
            let Some(&last) = lane.last() else { continue };
            let (a, b) = (&clips[last], &clips[i]);
            if a.end() <= b.position || crossfade(a, b).is_some() {
                lane.push(i);
                continue 'next;
            }
        }
        lanes.push(vec![i]);
    }
    lanes
}

/// What a lane's sequence holds.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Item {
    Filler(i64),
    Clip(usize),
    /// A transition of this length; `curve` the incoming side's.
    Transition {
        length: i64,
        cut: i64,
        level: Level,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Level {
    /// Into a clip: its fade-in.
    In(FadeCurve),
    /// Out of a clip into silence: its fade-out.
    Out(FadeCurve),
}

impl Level {
    fn at(self, t: f64) -> f64 {
        match self {
            Level::In(c) => c.gain(t),
            Level::Out(c) => 1.0 - c.gain(1.0 - t),
        }
    }
}

/// A lane as a sequence: fillers, clips and the transitions between them;
/// and its length.
pub(crate) fn sequence(clips: &[Clip], lane: &[usize]) -> (Vec<Item>, i64) {
    let mut items = Vec::new();
    let mut t = 0i64;
    let mut pending_out: Option<Fade> = None;
    for (k, &i) in lane.iter().enumerate() {
        let c = &clips[i];
        let (fade_in, fade_out) = fitted(c);
        let xfade = k.checked_sub(1).and_then(|p| crossfade(&clips[lane[p]], c));
        if let Some(overlap) = xfade {
            let curve = fade_in.map_or(FadeCurve::Linear, |f| f.curve);
            items.push(Item::Transition {
                length: overlap,
                cut: overlap / 2,
                level: Level::In(curve),
            });
            t -= overlap;
        } else {
            if let Some(out) = pending_out.take() {
                items.push(Item::Transition {
                    length: out.length,
                    cut: out.length,
                    level: Level::Out(out.curve),
                });
                t -= out.length;
            }
            let fi = fade_in.map_or(0, |f| f.length);
            let filler = c.position - t + fi;
            if filler > 0 {
                items.push(Item::Filler(filler));
                t += filler;
            }
            if let Some(f) = fade_in {
                items.push(Item::Transition {
                    length: f.length,
                    cut: 0,
                    level: Level::In(f.curve),
                });
                t -= f.length;
            }
        }
        items.push(Item::Clip(i));
        t += c.length;
        let next_crossfades = lane
            .get(k + 1)
            .is_some_and(|&n| crossfade(c, &clips[n]).is_some());
        pending_out = if next_crossfades { None } else { fade_out };
    }
    if let Some(out) = pending_out {
        items.push(Item::Transition {
            length: out.length,
            cut: out.length,
            level: Level::Out(out.curve),
        });
        items.push(Item::Filler(out.length));
    }
    (items, t)
}

/// One medium's mobs.
struct MediaIds {
    master: MobId,
    files: Vec<MobId>,
}

/// Write `comp` into `out`.
pub fn write<W: Write + Seek>(
    out: W,
    comp: &Composition,
    samples: &mut dyn Samples,
    mut options: WriteOptions,
    progress: &mut dyn FnMut(u64, u64) -> bool,
) -> Result<(W, Written), WriteError> {
    if comp.rate == 0 || comp.rate > i32::MAX as u32 {
        return Err(WriteError::Invalid("the composition has no rate".into()));
    }
    for m in &comp.media {
        if m.essence.len() != usize::from(m.channels) || m.channels == 0 || m.rate == 0 {
            return Err(WriteError::Invalid(format!(
                "{} does not say where each channel is",
                m.name
            )));
        }
    }
    for t in &comp.tracks {
        for c in &t.clips {
            match comp.media.get(c.media) {
                Some(m) if c.channel < m.channels => {}
                _ => {
                    return Err(WriteError::Invalid(format!(
                        "a clip of {} has no medium",
                        t.name
                    )));
                }
            }
        }
    }
    let when = options.when;
    let ids = &mut options.ids;
    let comp_id = ids.mob_id();
    let media_ids: Vec<MediaIds> = comp
        .media
        .iter()
        .map(|m| MediaIds {
            master: ids.mob_id(),
            files: (0..m.channels).map(|_| ids.mob_id()).collect(),
        })
        .collect();
    let generation = ids.uuid();

    let mut written = Written::default();
    let mut mobs = Vec::new();
    let mut essence = Vec::new();
    // Embedded streams: (medium, channel, frames, bits).
    let mut tokens: Vec<(usize, u16, u64, u16)> = Vec::new();

    for (m, (media, mids)) in comp.media.iter().zip(&media_ids).enumerate() {
        let mut master_slots = Vec::new();
        for (c, e) in media.essence.iter().enumerate() {
            let file_id = mids.files[c];
            let frames = media.frames.max(0);
            let descriptor = match e {
                Essence::Embed { bits } => {
                    let bits = if *bits == 16 { 16 } else { 24 };
                    let bytes = u64::from(bits / 8);
                    let len = frames as u64 * bytes;
                    let token = tokens.len();
                    tokens.push((m, c as u16, frames as u64, bits));
                    written.embedded += len;
                    essence.push(
                        Object::new(cls::ESSENCE_DATA)
                            .with(pid::ESSENCE_DATA_MOB_ID, Value::mob_id(file_id))
                            .with(
                                pid::ESSENCE_DATA_DATA,
                                Value::Stream(Stream::Pending { len, token }),
                            ),
                    );
                    Object::new(cls::PCM_DESCRIPTOR)
                        .with(pid::FILE_DESCRIPTOR_SAMPLE_RATE, edit_rate(media.rate))
                        .with(pid::FILE_DESCRIPTOR_LENGTH, Value::i64(frames))
                        .with(
                            pid::FILE_DESCRIPTOR_CONTAINER_FORMAT,
                            Value::weak(ty::CONTAINER_DEFINITION_WEAK, defs::CONTAINER_AAF),
                        )
                        .with(pid::SOUND_AUDIO_SAMPLING_RATE, edit_rate(media.rate))
                        .with(pid::SOUND_CHANNELS, Value::u32(1))
                        .with(pid::SOUND_QUANTIZATION_BITS, Value::u32(u32::from(bits)))
                        .with(pid::PCM_BLOCK_ALIGN, Value::u16(bits / 8))
                        .with(
                            pid::PCM_AVERAGE_BPS,
                            Value::u32(media.rate * u32::from(bits / 8)),
                        )
                }
                Essence::Link { url, wave_header } => {
                    let locator = Object::new(cls::NETWORK_LOCATOR)
                        .with(pid::NETWORK_LOCATOR_URL, Value::string(url));
                    Object::new(cls::WAVE_DESCRIPTOR)
                        .with(pid::FILE_DESCRIPTOR_SAMPLE_RATE, edit_rate(media.rate))
                        .with(pid::FILE_DESCRIPTOR_LENGTH, Value::i64(frames))
                        .with(
                            pid::FILE_DESCRIPTOR_CONTAINER_FORMAT,
                            Value::weak(ty::CONTAINER_DEFINITION_WEAK, defs::CONTAINER_AAF),
                        )
                        .with(pid::WAVE_SUMMARY, Value::Data(wave_header.clone()))
                        .with(
                            pid::ESSENCE_DESCRIPTOR_LOCATOR,
                            Value::StrongVector(vec![locator]),
                        )
                }
                Essence::Embedded(_) | Essence::Linked { .. } | Essence::Missing => {
                    return Err(WriteError::Invalid(format!(
                        "{} has essence that was read, not written",
                        media.name
                    )));
                }
            };
            let file_slot = timeline_slot(
                1,
                "",
                1,
                edit_rate(media.rate),
                source_clip(frames, 0, MobId::NIL, 0),
            );
            let name = if media.channels > 1 {
                format!("{}.A{}", media.name, c + 1)
            } else {
                media.name.clone()
            };
            mobs.push(
                mob(cls::SOURCE_MOB, file_id, &name, vec![file_slot], when).with(
                    pid::SOURCE_MOB_ESSENCE_DESCRIPTION,
                    Value::Strong(Box::new(descriptor)),
                ),
            );
            master_slots.push(timeline_slot(
                c as u32 + 1,
                "",
                c as u32 + 1,
                edit_rate(media.rate),
                source_clip(frames, 0, file_id, 1),
            ));
        }
        mobs.push(mob(
            cls::MASTER_MOB,
            mids.master,
            &media.name,
            master_slots,
            when,
        ));
    }

    // The composition.
    let rate = edit_rate(comp.rate);
    let mut slots = Vec::new();
    let mut slot_id = 1u32;
    let mut longest = 0i64;
    let mut sound = Vec::new();
    let mut number = 1u32;
    for track in &comp.tracks {
        let lanes = lanes(&track.clips);
        let count = lanes.len().max(1);
        for (n, lane) in lanes.iter().enumerate() {
            let (items, length) = sequence(&track.clips, lane);
            longest = longest.max(length);
            let mut components = Vec::with_capacity(items.len());
            for item in items {
                components.push(match item {
                    Item::Filler(len) => component(cls::FILLER, defs::DATA_SOUND, len),
                    Item::Clip(i) => {
                        let c = &track.clips[i];
                        written.clips += 1;
                        clip_segment(c, media_ids[c.media].master, u32::from(c.channel) + 1)
                    }
                    Item::Transition { length, cut, level } => {
                        written.transitions += 1;
                        transition(length, cut, |t| level.at(t))
                    }
                });
            }
            let seq = component(cls::SEQUENCE, defs::DATA_SOUND, length)
                .with(pid::SEQUENCE_COMPONENTS, Value::StrongVector(components));
            let name = if count > 1 && n > 0 {
                format!("{} ({})", track.name, n + 1)
            } else {
                track.name.clone()
            };
            sound.push((name, seq));
        }
        if lanes.is_empty() {
            let seq = component(cls::SEQUENCE, defs::DATA_SOUND, 0)
                .with(pid::SEQUENCE_COMPONENTS, Value::StrongVector(Vec::new()));
            sound.push((track.name.clone(), seq));
        }
    }
    if let Some(tc) = comp.timecode {
        let (num, den) = tc.rate;
        let frames = if num > 0 && den > 0 {
            (longest as i128 * i128::from(num)).div_euclid(i128::from(comp.rate) * i128::from(den))
                as i64
                + 1
        } else {
            0
        };
        let segment = component(cls::TIMECODE, defs::DATA_TIMECODE, frames)
            .with(pid::TIMECODE_START, Value::i64(tc.start))
            .with(pid::TIMECODE_FPS, Value::u16(tc.fps))
            .with(pid::TIMECODE_DROP, Value::bool(tc.drop));
        slots.push(timeline_slot(
            slot_id,
            "TC1",
            1,
            Value::rational(Rational::new(num, den)),
            segment,
        ));
        slot_id += 1;
    }
    written.slots = sound.len();
    for (name, seq) in sound {
        slots.push(timeline_slot(slot_id, &name, number, rate.clone(), seq));
        slot_id += 1;
        number += 1;
    }
    if !comp.markers.is_empty() {
        let mut markers: Vec<_> = comp.markers.iter().collect();
        markers.sort_by_key(|m| m.position);
        let events = markers
            .iter()
            .map(|m| {
                let mut o = Object::new(cls::DESCRIPTIVE_MARKER)
                    .with(
                        pid::COMPONENT_DATA_DEFINITION,
                        data_def(defs::DATA_DESCRIPTIVE_METADATA),
                    )
                    .with(pid::COMPONENT_LENGTH, Value::i64(1))
                    .with(pid::EVENT_POSITION, Value::i64(m.position))
                    .with(pid::EVENT_COMMENT, Value::string(&m.name));
                if let Some(c) = crate::model::property(cls::COMMENT_MARKER, "CommentMarkerUser") {
                    o.set(c.pid, Value::string("FaderFrame"));
                }
                if let (Some([r, g, b]), Some(p)) = (
                    m.color,
                    crate::model::property(cls::COMMENT_MARKER, "CommentMarkerColor"),
                ) {
                    let mut rgb = Vec::with_capacity(6);
                    for v in [r, g, b] {
                        rgb.extend_from_slice(&(u16::from(v) * 257).to_le_bytes());
                    }
                    o.set(p.pid, Value::Data(rgb));
                }
                o
            })
            .collect();
        let seq = Object::new(cls::SEQUENCE)
            .with(
                pid::COMPONENT_DATA_DEFINITION,
                data_def(defs::DATA_DESCRIPTIVE_METADATA),
            )
            .with(pid::SEQUENCE_COMPONENTS, Value::StrongVector(events));
        slots.push(
            Object::new(cls::EVENT_MOB_SLOT)
                .with(pid::SLOT_ID, Value::u32(1000 + slot_id))
                .with(pid::SLOT_NAME, Value::string("Markers"))
                .with(pid::EVENT_SLOT_EDIT_RATE, rate.clone())
                .with(pid::SLOT_SEGMENT, Value::Strong(Box::new(seq))),
        );
    }
    mobs.insert(
        0,
        mob(cls::COMPOSITION_MOB, comp_id, &comp.name, slots, when)
            .with(pid::MOB_USAGE_CODE, Value::auid(defs::USAGE_TOP_LEVEL)),
    );

    let mut content = Object::new(cls::CONTENT_STORAGE).with(
        pid::CONTENT_MOBS,
        Value::StrongSet {
            key_pid: pid::MOB_MOB_ID,
            key_from: pid::MOB_MOB_ID,
            key_size: 32,
            items: mobs,
        },
    );
    if !essence.is_empty() {
        content.set(
            pid::CONTENT_ESSENCE_DATA,
            Value::StrongSet {
                key_pid: pid::ESSENCE_DATA_MOB_ID,
                key_from: pid::ESSENCE_DATA_MOB_ID,
                key_size: 32,
                items: essence,
            },
        );
    }
    let header = Object::new(cls::HEADER)
        .with(pid::HEADER_BYTE_ORDER, Value::u16(0x4949))
        .with(pid::HEADER_LAST_MODIFIED, Value::timestamp(when))
        .with(pid::HEADER_CONTENT, Value::Strong(Box::new(content)))
        .with(
            pid::HEADER_DICTIONARY,
            Value::Strong(Box::new(dictionary())),
        )
        .with(pid::HEADER_VERSION, Value::Data(vec![1, 2]))
        .with(
            pid::HEADER_IDENTIFICATION_LIST,
            Value::StrongVector(vec![identification(&options, generation)]),
        )
        .with(pid::HEADER_OBJECT_MODEL_VERSION, Value::u32(1))
        .with(
            pid::HEADER_OPERATIONAL_PATTERN,
            Value::auid(defs::OP_EDIT_PROTOCOL),
        );
    let root = Object::new(cls::ROOT)
        .with(
            pid::ROOT_META_DICTIONARY,
            Value::Strong(Box::new(crate::meta::meta_dictionary())),
        )
        .with(pid::ROOT_HEADER, Value::Strong(Box::new(header)));

    // The essence, converted as it is written.
    let total = written.embedded;
    let mut done = 0u64;
    let mut cancelled = false;
    let mut fill = |token: usize, w: &mut dyn Write| -> io::Result<()> {
        let Some(&(m, c, frames, bits)) = tokens.get(token) else {
            return Ok(());
        };
        const CHUNK: usize = 1 << 16;
        let mut buf = vec![0f32; CHUNK];
        let mut bytes = Vec::with_capacity(CHUNK * 3);
        let mut dither = Tpdf::new(token as u64 + 1);
        let mut at = 0u64;
        while at < frames {
            let n = CHUNK.min((frames - at) as usize);
            samples.read(m, c, at, &mut buf[..n])?;
            bytes.clear();
            for &x in &buf[..n] {
                let x = if x.is_finite() { x } else { 0.0 };
                if bits == 16 {
                    let v = (f64::from(x) * 32768.0 + dither.next()).round();
                    bytes.extend_from_slice(&(v.clamp(-32768.0, 32767.0) as i16).to_le_bytes());
                } else {
                    let v = (f64::from(x) * 8_388_608.0).round();
                    let v = v.clamp(-8_388_608.0, 8_388_607.0) as i32;
                    bytes.extend_from_slice(&v.to_le_bytes()[..3]);
                }
            }
            w.write_all(&bytes)?;
            at += n as u64;
            done += bytes.len() as u64;
            if !progress(done, total) {
                cancelled = true;
                return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
            }
        }
        Ok(())
    };
    let cfb = CfbWriter::new(out, options.sector_size).map_err(StoreError::from)?;
    let result = StoreWriter::new(cfb, &mut fill).write_root(&root);
    match result {
        Ok(out) => Ok((out, written)),
        Err(_) if cancelled => Err(WriteError::Cancelled),
        Err(e) => Err(e.into()),
    }
}

/// Triangular dither of one LSB peak (16-bit essence).
struct Tpdf(u64);

impl Tpdf {
    fn new(seed: u64) -> Tpdf {
        Tpdf(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }

    fn uniform(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }

    fn next(&mut self) -> f64 {
        self.uniform() - self.uniform()
    }
}

#[cfg(test)]
mod tests;
