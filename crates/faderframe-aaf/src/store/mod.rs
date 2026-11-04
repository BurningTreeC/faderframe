//! AAF's stored form ("structured storage binding"): every object a
//! storage whose class id is the object's class, its properties in a
//! `properties` stream (local id, stored form, size, then the values),
//! strongly referenced objects in child storages, collections of them as
//! numbered storages with an `index` stream, weak references as a key and
//! a tag into the root's `referenced properties` (the property paths from
//! the root to the sets they point into), streams as child streams.
//!
//! [`Object`] is one object with its properties in that form, whole:
//! what [`write`] puts into a compound file and [`read`] takes out.

pub mod dump;
pub mod read;
pub mod write;

use crate::ids::{Auid, MobId};

/// Stored forms ("SF_…").
pub(crate) mod sf {
    pub const DATA: u16 = 0x82;
    pub const DATA_STREAM: u16 = 0x42;
    pub const STRONG: u16 = 0x22;
    pub const STRONG_VECTOR: u16 = 0x32;
    pub const STRONG_SET: u16 = 0x3a;
    pub const WEAK: u16 = 0x02;
    pub const WEAK_VECTOR: u16 = 0x12;
    pub const WEAK_SET: u16 = 0x1a;
}

/// One object: its class and its properties.
#[derive(Clone, Debug, PartialEq)]
pub struct Object {
    pub class: Auid,
    pub props: Vec<Prop>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Prop {
    pub pid: u16,
    pub value: Value,
}

/// A property's value as stored.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// Plain data (numbers, strings, records, arrays, AUIDs, MobIDs …).
    Data(Vec<u8>),
    Stream(Stream),
    Strong(Box<Object>),
    StrongVector(Vec<Object>),
    /// A set of objects told apart by a key: the value of their
    /// `key_from` property, recorded as property `key_pid`.
    StrongSet {
        key_pid: u16,
        key_from: u16,
        key_size: u8,
        items: Vec<Object>,
    },
    Weak(WeakRef),
    WeakVector(WeakRefs),
    WeakSet(WeakRefs),
}

/// A stream property's bytes.
#[derive(Clone, Debug, PartialEq)]
pub enum Stream {
    Bytes(Vec<u8>),
    /// Filled by the writer's stream source when written (`token` names
    /// it to the source).
    Pending {
        len: u64,
        token: usize,
    },
    /// In a file being read: its compound file entry.
    InFile {
        entry: usize,
        len: u64,
    },
}

impl Stream {
    pub fn len(&self) -> u64 {
        match self {
            Stream::Bytes(b) => b.len() as u64,
            Stream::Pending { len, .. } | Stream::InFile { len, .. } => *len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A weak reference: the key of an object in the set at `path` (local
/// property ids from the root).
#[derive(Clone, Debug, PartialEq)]
pub struct WeakRef {
    pub path: Vec<u16>,
    pub key_pid: u16,
    pub key: Vec<u8>,
}

/// Several weak references into one set.
#[derive(Clone, Debug, PartialEq)]
pub struct WeakRefs {
    pub path: Vec<u16>,
    pub key_pid: u16,
    pub key_size: u8,
    pub keys: Vec<Vec<u8>>,
}

impl Object {
    pub fn new(class: Auid) -> Object {
        Object {
            class,
            props: Vec::new(),
        }
    }

    /// Set (or replace) a property.
    pub fn set(&mut self, pid: u16, value: Value) {
        match self.props.iter_mut().find(|p| p.pid == pid) {
            Some(p) => p.value = value,
            None => self.props.push(Prop { pid, value }),
        }
    }

    pub fn with(mut self, pid: u16, value: Value) -> Object {
        self.set(pid, value);
        self
    }

    pub fn get(&self, pid: u16) -> Option<&Value> {
        self.props.iter().find(|p| p.pid == pid).map(|p| &p.value)
    }

    pub fn data(&self, pid: u16) -> Option<&[u8]> {
        match self.get(pid)? {
            Value::Data(d) => Some(d),
            _ => None,
        }
    }

    pub fn u8(&self, pid: u16) -> Option<u8> {
        self.data(pid)?.first().copied()
    }

    pub fn u16(&self, pid: u16) -> Option<u16> {
        Some(u16::from_le_bytes(
            self.data(pid)?.get(..2)?.try_into().ok()?,
        ))
    }

    pub fn u32(&self, pid: u16) -> Option<u32> {
        Some(u32::from_le_bytes(
            self.data(pid)?.get(..4)?.try_into().ok()?,
        ))
    }

    pub fn i32(&self, pid: u16) -> Option<i32> {
        self.u32(pid).map(|v| v as i32)
    }

    pub fn i64(&self, pid: u16) -> Option<i64> {
        Some(i64::from_le_bytes(
            self.data(pid)?.get(..8)?.try_into().ok()?,
        ))
    }

    pub fn bool(&self, pid: u16) -> Option<bool> {
        self.u8(pid).map(|v| v != 0)
    }

    pub fn string(&self, pid: u16) -> Option<String> {
        Some(decode_string(self.data(pid)?))
    }

    pub fn auid(&self, pid: u16) -> Option<Auid> {
        Auid::from_le(self.data(pid)?)
    }

    pub fn mob_id(&self, pid: u16) -> Option<MobId> {
        MobId::from_le(self.data(pid)?)
    }

    pub fn rational(&self, pid: u16) -> Option<Rational> {
        Rational::decode(self.data(pid)?)
    }

    pub fn strong(&self, pid: u16) -> Option<&Object> {
        match self.get(pid)? {
            Value::Strong(o) => Some(o),
            _ => None,
        }
    }

    /// The objects of a strong vector or set.
    pub fn objects(&self, pid: u16) -> &[Object] {
        match self.get(pid) {
            Some(Value::StrongVector(v)) => v,
            Some(Value::StrongSet { items, .. }) => items,
            _ => &[],
        }
    }

    /// The key a weak reference points at, as an AUID (definitions).
    pub fn weak_auid(&self, pid: u16) -> Option<Auid> {
        match self.get(pid)? {
            Value::Weak(w) => Auid::from_le(&w.key),
            _ => None,
        }
    }

    pub fn stream(&self, pid: u16) -> Option<&Stream> {
        match self.get(pid)? {
            Value::Stream(s) => Some(s),
            _ => None,
        }
    }
}

/// A rational number (edit rates, times inside an operation).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rational {
    pub num: i32,
    pub den: i32,
}

impl Rational {
    pub const fn new(num: i32, den: i32) -> Rational {
        Rational { num, den }
    }

    pub fn of_f64(v: f64) -> Rational {
        // Six decimals are more than any reader shows.
        let den = 1_000_000;
        let num = (v * f64::from(den))
            .round()
            .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
        Rational { num, den }
    }

    pub fn to_f64(self) -> f64 {
        if self.den == 0 {
            0.0
        } else {
            f64::from(self.num) / f64::from(self.den)
        }
    }

    pub fn encode(self) -> Vec<u8> {
        let mut b = self.num.to_le_bytes().to_vec();
        b.extend_from_slice(&self.den.to_le_bytes());
        b
    }

    pub fn decode(b: &[u8]) -> Option<Rational> {
        Some(Rational {
            num: i32::from_le_bytes(b.get(..4)?.try_into().ok()?),
            den: i32::from_le_bytes(b.get(4..8)?.try_into().ok()?),
        })
    }
}

/// A date and time as AAF stores one (TimeStamp).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TimeStamp {
    pub year: i16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl TimeStamp {
    /// Now, in UTC.
    pub fn now() -> TimeStamp {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()) as i64;
        let days = secs.div_euclid(86_400);
        let rest = secs.rem_euclid(86_400);
        // Civil from days (Howard Hinnant's algorithm).
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + i64::from(month <= 2);
        TimeStamp {
            year: year as i16,
            month: month as u8,
            day: day as u8,
            hour: (rest / 3600) as u8,
            minute: (rest / 60 % 60) as u8,
            second: (rest % 60) as u8,
        }
    }

    pub fn encode(self) -> Vec<u8> {
        let mut b = self.year.to_le_bytes().to_vec();
        b.extend_from_slice(&[self.month, self.day, self.hour, self.minute, self.second, 0]);
        b
    }
}

/// A string as stored: UTF-16LE with a terminating zero.
pub fn encode_string(s: &str) -> Vec<u8> {
    let mut b = Vec::with_capacity(s.len() * 2 + 2);
    for u in s.encode_utf16() {
        b.extend_from_slice(&u.to_le_bytes());
    }
    b.extend_from_slice(&[0, 0]);
    b
}

/// A stored string (up to its zero, if any).
pub fn decode_string(b: &[u8]) -> String {
    let units: Vec<u16> = b
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// Several stored strings one after another (a string array).
pub fn decode_strings(b: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = Vec::new();
    for c in b.as_chunks::<2>().0 {
        let u = u16::from_le_bytes(*c);
        if u == 0 {
            out.push(String::from_utf16_lossy(&cur));
            cur.clear();
        } else {
            cur.push(u);
        }
    }
    out
}

/// Value constructors for the types this crate writes.
impl Value {
    pub fn string(s: &str) -> Value {
        Value::Data(encode_string(s))
    }

    pub fn u8(v: u8) -> Value {
        Value::Data(vec![v])
    }

    pub fn bool(v: bool) -> Value {
        Value::Data(vec![u8::from(v)])
    }

    pub fn u16(v: u16) -> Value {
        Value::Data(v.to_le_bytes().to_vec())
    }

    pub fn u32(v: u32) -> Value {
        Value::Data(v.to_le_bytes().to_vec())
    }

    pub fn i32(v: i32) -> Value {
        Value::Data(v.to_le_bytes().to_vec())
    }

    pub fn i64(v: i64) -> Value {
        Value::Data(v.to_le_bytes().to_vec())
    }

    pub fn auid(a: Auid) -> Value {
        Value::Data(a.to_le().to_vec())
    }

    pub fn mob_id(m: MobId) -> Value {
        Value::Data(m.to_le().to_vec())
    }

    pub fn rational(r: Rational) -> Value {
        Value::Data(r.encode())
    }

    pub fn timestamp(t: TimeStamp) -> Value {
        Value::Data(t.encode())
    }

    /// A value of any type (indirect): byte order, the type, the value.
    pub fn indirect(ty: Auid, value: &[u8]) -> Value {
        let mut b = vec![0x4c];
        b.extend_from_slice(&ty.to_le());
        b.extend_from_slice(value);
        Value::Data(b)
    }

    /// A weak reference of type `ty` (one of [`crate::model::ty`]) to the
    /// definition `key`.
    pub fn weak(ty: Auid, key: Auid) -> Value {
        Value::Weak(WeakRef {
            path: crate::model::weak_path(ty).unwrap_or_default(),
            key_pid: weak_key_pid(ty),
            key: key.to_le().to_vec(),
        })
    }

    /// Weak references of a set or vector type `ty` to definitions.
    pub fn weak_refs(ty: Auid, keys: &[Auid]) -> WeakRefs {
        WeakRefs {
            path: crate::model::weak_path(ty).unwrap_or_default(),
            key_pid: weak_key_pid(ty),
            key_size: 16,
            keys: keys.iter().map(|k| k.to_le().to_vec()).collect(),
        }
    }
}

/// The property weak references of a type are keyed by: meta definitions'
/// and definitions' Identification, mobs' MobID.
fn weak_key_pid(ty: Auid) -> u16 {
    use crate::model::pid;
    match crate::model::weak_path(ty).as_deref() {
        Some([0x0001, ..]) => pid::META_IDENTIFICATION,
        Some([0x0002, 0x3b03, 0x1901]) => pid::MOB_MOB_ID,
        _ => pid::DEFINITION_IDENTIFICATION,
    }
}

/// A property's directory name: its name, squeezed to fit, a dash and its
/// local id in hex (as the AAF SDK and pyaaf2 name them).
pub(crate) fn mangle(name: &str, pid: u16, size: usize) -> String {
    let hex = format!("{pid:x}");
    let max = size.saturating_sub(hex.len() + 2);
    let chars: Vec<char> = name.chars().collect();
    let squeezed: String = if chars.len() <= max {
        name.to_string()
    } else {
        let half = max / 2;
        (0..max)
            .map(|i| {
                if i < half {
                    chars[i]
                } else if i == half {
                    '-'
                } else {
                    chars[chars.len() - (max - i)]
                }
            })
            .collect()
    };
    format!("{squeezed}-{hex}")
}

#[cfg(test)]
mod tests;
