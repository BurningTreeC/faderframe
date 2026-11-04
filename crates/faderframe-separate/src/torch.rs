//! PyTorch checkpoints (`torch.save`'s zip format) read without running
//! them.
//!
//! A checkpoint is a zip archive of stored (uncompressed) entries: a
//! pickle (`<name>/data.pkl`) describing the saved object, and one entry
//! per tensor storage (`<name>/data/<key>`), little-endian. The pickle is
//! evaluated by a small machine that knows the opcodes `torch.save` writes
//! and builds plain values: tensors become [`Tensor`] descriptions of
//! their storage, every other class a named [`Value::Object`] holding its
//! arguments — nothing a pickle names is ever called, so a checkpoint can
//! carry data and nothing else.

use crate::SeparateError;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Element types of storages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DType {
    F32,
    F64,
    F16,
    BF16,
    I64,
    I32,
    I16,
    I8,
    U8,
    Bool,
}

impl DType {
    /// The storage class `torch.save` names (`torch.FloatStorage` …).
    fn from_storage(name: &str) -> Option<Self> {
        Some(match name {
            "FloatStorage" => Self::F32,
            "DoubleStorage" => Self::F64,
            "HalfStorage" => Self::F16,
            "BFloat16Storage" => Self::BF16,
            "LongStorage" => Self::I64,
            "IntStorage" => Self::I32,
            "ShortStorage" => Self::I16,
            "CharStorage" => Self::I8,
            "ByteStorage" => Self::U8,
            "BoolStorage" => Self::Bool,
            _ => return None,
        })
    }

    /// Bytes per element.
    pub fn size(self) -> usize {
        match self {
            Self::F64 | Self::I64 => 8,
            Self::F32 | Self::I32 => 4,
            Self::F16 | Self::BF16 | Self::I16 => 2,
            Self::I8 | Self::U8 | Self::Bool => 1,
        }
    }
}

/// A tensor as the pickle describes it: a view of a storage.
#[derive(Clone, Debug, PartialEq)]
pub struct Tensor {
    pub dtype: DType,
    /// The storage's key (its entry `data/<key>`).
    pub storage: String,
    /// Offset into the storage, in elements.
    pub offset: usize,
    pub shape: Vec<usize>,
    pub stride: Vec<usize>,
}

impl Tensor {
    pub fn len(&self) -> usize {
        self.shape.iter().product()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// What the pickle builds.
#[derive(Clone, Debug)]
pub enum Value {
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    Tuple(Rc<Vec<Value>>),
    List(Rc<RefCell<Vec<Value>>>),
    /// In insertion order (an `OrderedDict` as well).
    Dict(Rc<RefCell<Vec<(Value, Value)>>>),
    /// A class or function the pickle names (`module.name`).
    Global(String),
    /// What calling a global would make: its name, its arguments and the
    /// state set on it.
    Object(Rc<RefCell<Object>>),
    /// A storage reference (`BINPERSID`).
    Storage {
        dtype: DType,
        key: String,
    },
    Tensor(Tensor),
}

#[derive(Clone, Debug)]
pub struct Object {
    pub class: String,
    pub args: Value,
    pub state: Option<Value>,
}

impl Value {
    /// A dictionary's value under a string key.
    pub fn get(&self, key: &str) -> Option<Value> {
        match self {
            Value::Dict(d) => d
                .borrow()
                .iter()
                .find(|(k, _)| matches!(k, Value::Str(s) if s == key))
                .map(|(_, v)| v.clone()),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            Value::Bool(b) => Some(i64::from(*b)),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            Value::Int(i) => Some(*i != 0),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The items of a list or tuple.
    pub fn items(&self) -> Option<Vec<Value>> {
        match self {
            Value::List(l) => Some(l.borrow().clone()),
            Value::Tuple(t) => Some(t.as_ref().clone()),
            _ => None,
        }
    }

    /// A dictionary's entries keyed by strings (others left out).
    pub fn entries(&self) -> Vec<(String, Value)> {
        match self {
            Value::Dict(d) => d
                .borrow()
                .iter()
                .filter_map(|(k, v)| Some((k.as_str()?.to_string(), v.clone())))
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// A stored zip entry: where its data starts and how long it is.
#[derive(Clone, Copy, Debug)]
struct Entry {
    data: u64,
    len: u64,
}

/// An opened checkpoint: the object it holds, its storages on disk.
pub struct Checkpoint {
    path: PathBuf,
    entries: HashMap<String, Entry>,
    /// The archive's top folder (`archive/`, or the file's stem).
    prefix: String,
    pub root: Value,
}

fn bad(path: &Path, what: impl std::fmt::Display) -> SeparateError {
    SeparateError::Format(format!("{}: {what}", path.display()))
}

impl Checkpoint {
    pub fn open(path: &Path) -> Result<Self, SeparateError> {
        let io = |e| SeparateError::Io(path.display().to_string(), e);
        let mut file = File::open(path).map_err(io)?;
        let entries = zip_entries(&mut file, path)?;
        let pickle_name = entries
            .keys()
            .find(|n| n.ends_with("/data.pkl") || *n == "data.pkl")
            .cloned()
            .ok_or_else(|| bad(path, "not a PyTorch checkpoint (no data.pkl)"))?;
        let prefix = pickle_name.trim_end_matches("data.pkl").to_string();
        if let Some(order) = entries.get(&format!("{prefix}byteorder")) {
            let bytes = read_entry(&mut file, *order, path)?;
            if bytes.trim_ascii() != b"little" {
                return Err(bad(path, "big-endian storages"));
            }
        }
        let pickle = read_entry(&mut file, entries[&pickle_name], path)?;
        let root = Machine::new(&pickle).run().map_err(|e| bad(path, e))?;
        Ok(Self {
            path: path.to_path_buf(),
            entries,
            prefix,
            root,
        })
    }

    /// A tensor's elements as f32 (floating types converted; integers
    /// refused), in row-major order whatever its strides.
    pub fn read_f32(&self, t: &Tensor) -> Result<Vec<f32>, SeparateError> {
        let entry = self
            .entries
            .get(&format!("{}data/{}", self.prefix, t.storage))
            .copied()
            .ok_or_else(|| bad(&self.path, format!("storage {} missing", t.storage)))?;
        let size = t.dtype.size();
        // The span of the storage the view reaches.
        let reach = if t.is_empty() {
            0
        } else {
            1 + t
                .shape
                .iter()
                .zip(&t.stride)
                .map(|(n, s)| (n - 1) * s)
                .sum::<usize>()
        };
        let (from, bytes) = (t.offset * size, reach * size);
        if (from + bytes) as u64 > entry.len {
            return Err(bad(&self.path, format!("storage {} too short", t.storage)));
        }
        let mut file = File::open(&self.path)
            .map_err(|e| SeparateError::Io(self.path.display().to_string(), e))?;
        let raw = read_range(&mut file, entry.data + from as u64, bytes, &self.path)?;
        let at = |i: usize| -> Result<f32, SeparateError> {
            let b = &raw[i * size..(i + 1) * size];
            Ok(match t.dtype {
                DType::F32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                DType::F64 => {
                    f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as f32
                }
                DType::F16 => f16_to_f32(u16::from_le_bytes([b[0], b[1]])),
                DType::BF16 => f32::from_bits(u32::from(u16::from_le_bytes([b[0], b[1]])) << 16),
                other => {
                    return Err(bad(&self.path, format!("{other:?} tensor read as floats")));
                }
            })
        };
        let contiguous = contiguous_strides(&t.shape);
        if t.stride == contiguous {
            return (0..t.len()).map(at).collect();
        }
        // A strided view: walk it in row-major order.
        let mut out = Vec::with_capacity(t.len());
        let mut index = vec![0usize; t.shape.len()];
        for _ in 0..t.len() {
            let pos: usize = index.iter().zip(&t.stride).map(|(i, s)| i * s).sum();
            out.push(at(pos)?);
            for d in (0..index.len()).rev() {
                index[d] += 1;
                if index[d] < t.shape[d] {
                    break;
                }
                index[d] = 0;
            }
        }
        Ok(out)
    }
}

fn contiguous_strides(shape: &[usize]) -> Vec<usize> {
    let mut s = vec![1usize; shape.len()];
    for d in (0..shape.len().saturating_sub(1)).rev() {
        s[d] = s[d + 1] * shape[d + 1];
    }
    s
}

pub(crate) fn f16_to_f32(h: u16) -> f32 {
    let sign = u32::from(h >> 15) << 31;
    let exp = u32::from((h >> 10) & 0x1f);
    let frac = u32::from(h & 0x3ff);
    let bits = match exp {
        0 if frac == 0 => sign,
        0 => {
            let mut e = 127 - 15 + 1;
            let mut f = frac;
            while f & 0x400 == 0 {
                f <<= 1;
                e -= 1;
            }
            sign | (e << 23) | ((f & 0x3ff) << 13)
        }
        31 => sign | 0x7f80_0000 | (frac << 13),
        _ => sign | ((exp + 127 - 15) << 23) | (frac << 13),
    };
    f32::from_bits(bits)
}

fn read_range(file: &mut File, at: u64, len: usize, path: &Path) -> Result<Vec<u8>, SeparateError> {
    let io = |e| SeparateError::Io(path.display().to_string(), e);
    file.seek(SeekFrom::Start(at)).map_err(io)?;
    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf).map_err(io)?;
    Ok(buf)
}

fn read_entry(file: &mut File, e: Entry, path: &Path) -> Result<Vec<u8>, SeparateError> {
    let len = usize::try_from(e.len).map_err(|_| bad(path, "entry too large"))?;
    read_range(file, e.data, len, path)
}

fn u16_at(b: &[u8], i: usize) -> u64 {
    u64::from(u16::from_le_bytes([b[i], b[i + 1]]))
}

fn u32_at(b: &[u8], i: usize) -> u64 {
    u64::from(u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]))
}

fn u64_at(b: &[u8], i: usize) -> u64 {
    u64::from_le_bytes([
        b[i],
        b[i + 1],
        b[i + 2],
        b[i + 3],
        b[i + 4],
        b[i + 5],
        b[i + 6],
        b[i + 7],
    ])
}

/// The archive's entries from its central directory (ZIP64 too). Only
/// stored entries are taken: `torch.save` compresses nothing.
fn zip_entries(file: &mut File, path: &Path) -> Result<HashMap<String, Entry>, SeparateError> {
    let io = |e| SeparateError::Io(path.display().to_string(), e);
    let size = file.seek(SeekFrom::End(0)).map_err(io)?;
    let tail_len = size.min(65_557 + 20) as usize;
    let tail = read_range(file, size - tail_len as u64, tail_len, path)?;
    let eocd = (0..tail_len.saturating_sub(21))
        .rev()
        .find(|&i| tail[i..i + 4] == [0x50, 0x4b, 0x05, 0x06])
        .ok_or_else(|| bad(path, "not a zip archive"))?;
    let mut count = u16_at(&tail, eocd + 10);
    let mut dir_len = u32_at(&tail, eocd + 12);
    let mut dir_at = u32_at(&tail, eocd + 16);
    // ZIP64: the locator sits right before the end record.
    if eocd >= 20 && tail[eocd - 20..eocd - 16] == [0x50, 0x4b, 0x06, 0x07] {
        let record_at = u64_at(&tail, eocd - 20 + 8);
        let record = read_range(file, record_at, 56, path)?;
        if record[0..4] != [0x50, 0x4b, 0x06, 0x06] {
            return Err(bad(path, "damaged ZIP64 record"));
        }
        count = u64_at(&record, 32);
        dir_len = u64_at(&record, 40);
        dir_at = u64_at(&record, 48);
    }
    let dir_len = usize::try_from(dir_len).map_err(|_| bad(path, "directory too large"))?;
    let dir = read_range(file, dir_at, dir_len, path)?;
    let mut entries = HashMap::new();
    let mut i = 0usize;
    for _ in 0..count {
        if i + 46 > dir.len() || dir[i..i + 4] != [0x50, 0x4b, 0x01, 0x02] {
            return Err(bad(path, "damaged zip directory"));
        }
        let method = u16_at(&dir, i + 10);
        let mut csize = u32_at(&dir, i + 20);
        let mut usize_ = u32_at(&dir, i + 24);
        let name_len = u16_at(&dir, i + 28) as usize;
        let extra_len = u16_at(&dir, i + 30) as usize;
        let comment_len = u16_at(&dir, i + 32) as usize;
        let mut local = u32_at(&dir, i + 42);
        let name = String::from_utf8_lossy(&dir[i + 46..i + 46 + name_len]).to_string();
        // ZIP64 sizes and offset in the extra field, in this order, for
        // the fields that overflowed.
        let extra = &dir[i + 46 + name_len..(i + 46 + name_len + extra_len).min(dir.len())];
        let mut e = 0usize;
        while e + 4 <= extra.len() {
            let (id, len) = (u16_at(extra, e), u16_at(extra, e + 2) as usize);
            if id == 1 {
                let mut f = e + 4;
                for field in [&mut usize_, &mut csize, &mut local] {
                    if *field == 0xffff_ffff && f + 8 <= e + 4 + len {
                        *field = u64_at(extra, f);
                        f += 8;
                    }
                }
            }
            e += 4 + len;
        }
        if method != 0 || csize != usize_ {
            return Err(bad(
                path,
                format!("{name}: compressed entries are not read"),
            ));
        }
        // The data starts after the local header, whose extra field (the
        // alignment padding) may differ from the directory's.
        let head = read_range(file, local, 30, path)?;
        if head[0..4] != [0x50, 0x4b, 0x03, 0x04] {
            return Err(bad(path, format!("{name}: damaged local header")));
        }
        let data = local + 30 + u16_at(&head, 26) + u16_at(&head, 28);
        entries.insert(name, Entry { data, len: usize_ });
        i += 46 + name_len + extra_len + comment_len;
    }
    Ok(entries)
}

/// The pickle machine.
struct Machine<'a> {
    data: &'a [u8],
    at: usize,
    stack: Vec<Value>,
    marks: Vec<usize>,
    memo: HashMap<u64, Value>,
}

impl<'a> Machine<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            at: 0,
            stack: Vec::new(),
            marks: Vec::new(),
            memo: HashMap::new(),
        }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self
            .at
            .checked_add(n)
            .filter(|e| *e <= self.data.len())
            .ok_or("the pickle ends early")?;
        let s = &self.data[self.at..end];
        self.at = end;
        Ok(s)
    }

    fn byte(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u64, String> {
        Ok(u16_at(self.take(2)?, 0))
    }

    fn u32(&mut self) -> Result<u64, String> {
        Ok(u32_at(self.take(4)?, 0))
    }

    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64_at(self.take(8)?, 0))
    }

    fn line(&mut self) -> Result<String, String> {
        let rest = &self.data[self.at..];
        let n = rest
            .iter()
            .position(|b| *b == b'\n')
            .ok_or("the pickle ends early")?;
        let s = String::from_utf8_lossy(&rest[..n]).to_string();
        self.at += n + 1;
        Ok(s)
    }

    fn sized(&mut self, n: u64) -> Result<&'a [u8], String> {
        self.take(usize::try_from(n).map_err(|_| "a string too long")?)
    }

    fn pop(&mut self) -> Result<Value, String> {
        self.stack
            .pop()
            .ok_or_else(|| "the pickle's stack ran empty".into())
    }

    fn top(&mut self) -> Result<&mut Value, String> {
        self.stack
            .last_mut()
            .ok_or_else(|| "the pickle's stack ran empty".into())
    }

    /// Everything since the last mark.
    fn pop_mark(&mut self) -> Result<Vec<Value>, String> {
        let m = self.marks.pop().ok_or("a mark is missing")?;
        if m > self.stack.len() {
            return Err("a mark is missing".into());
        }
        Ok(self.stack.split_off(m))
    }

    fn run(mut self) -> Result<Value, String> {
        loop {
            let op = self.byte()?;
            match op {
                0x80 => {
                    self.byte()?; // PROTO
                }
                0x95 => {
                    self.u64()?; // FRAME
                }
                b'.' => return self.pop(),
                b'(' => self.marks.push(self.stack.len()),
                b'0' => {
                    self.pop()?;
                }
                b'1' => {
                    self.pop_mark()?;
                }
                b'2' => {
                    let v = self.top()?.clone();
                    self.stack.push(v);
                }
                b'N' => self.stack.push(Value::None),
                0x88 => self.stack.push(Value::Bool(true)),
                0x89 => self.stack.push(Value::Bool(false)),
                b'J' => {
                    let b = self.take(4)?;
                    self.stack.push(Value::Int(i64::from(i32::from_le_bytes([
                        b[0], b[1], b[2], b[3],
                    ]))));
                }
                b'K' => {
                    let v = self.byte()?;
                    self.stack.push(Value::Int(i64::from(v)));
                }
                b'M' => {
                    let v = self.u16()?;
                    self.stack.push(Value::Int(v as i64));
                }
                0x8a | 0x8b => {
                    let n = if op == 0x8a {
                        u64::from(self.byte()?)
                    } else {
                        self.u32()?
                    };
                    let b = self.sized(n)?;
                    if b.len() > 8 {
                        return Err("an integer wider than 64 bits".into());
                    }
                    let mut v: i64 = 0;
                    for (i, x) in b.iter().enumerate() {
                        v |= i64::from(*x) << (8 * i);
                    }
                    // Sign-extend from the top byte.
                    if let Some(last) = b.last()
                        && last & 0x80 != 0
                        && b.len() < 8
                    {
                        v -= 1i64 << (8 * b.len());
                    }
                    self.stack.push(Value::Int(v));
                }
                b'G' => {
                    let b = self.take(8)?;
                    let mut a = [0u8; 8];
                    a.copy_from_slice(b);
                    self.stack.push(Value::Float(f64::from_be_bytes(a)));
                }
                b'X' | 0x8c | 0x8d => {
                    let n = match op {
                        b'X' => self.u32()?,
                        0x8c => u64::from(self.byte()?),
                        _ => self.u64()?,
                    };
                    let s = String::from_utf8_lossy(self.sized(n)?).to_string();
                    self.stack.push(Value::Str(s));
                }
                b'T' | b'U' => {
                    let n = if op == b'T' {
                        self.u32()?
                    } else {
                        u64::from(self.byte()?)
                    };
                    let s = self.sized(n)?.iter().map(|b| char::from(*b)).collect();
                    self.stack.push(Value::Str(s));
                }
                b'B' | b'C' | 0x8e => {
                    let n = match op {
                        b'B' => self.u32()?,
                        b'C' => u64::from(self.byte()?),
                        _ => self.u64()?,
                    };
                    let b = self.sized(n)?.to_vec();
                    self.stack.push(Value::Bytes(b));
                }
                b')' => self.stack.push(Value::Tuple(Rc::new(Vec::new()))),
                b't' => {
                    let items = self.pop_mark()?;
                    self.stack.push(Value::Tuple(Rc::new(items)));
                }
                0x85..=0x87 => {
                    let n = usize::from(op - 0x84);
                    if self.stack.len() < n {
                        return Err("the pickle's stack ran empty".into());
                    }
                    let items = self.stack.split_off(self.stack.len() - n);
                    self.stack.push(Value::Tuple(Rc::new(items)));
                }
                b']' => self
                    .stack
                    .push(Value::List(Rc::new(RefCell::new(Vec::new())))),
                b'l' => {
                    let items = self.pop_mark()?;
                    self.stack.push(Value::List(Rc::new(RefCell::new(items))));
                }
                0x8f => self
                    .stack
                    .push(Value::List(Rc::new(RefCell::new(Vec::new())))),
                0x91 => {
                    let items = self.pop_mark()?;
                    self.stack.push(Value::List(Rc::new(RefCell::new(items))));
                }
                b'a' => {
                    let v = self.pop()?;
                    match self.top()? {
                        Value::List(l) => l.borrow_mut().push(v),
                        _ => return Err("APPEND to a non-list".into()),
                    }
                }
                b'e' | 0x90 => {
                    let items = self.pop_mark()?;
                    match self.top()? {
                        Value::List(l) => l.borrow_mut().extend(items),
                        _ => return Err("APPENDS to a non-list".into()),
                    }
                }
                b'}' => self
                    .stack
                    .push(Value::Dict(Rc::new(RefCell::new(Vec::new())))),
                b'd' => {
                    let items = self.pop_mark()?;
                    let pairs = pairs(items)?;
                    self.stack.push(Value::Dict(Rc::new(RefCell::new(pairs))));
                }
                b's' => {
                    let v = self.pop()?;
                    let k = self.pop()?;
                    set_items(self.top()?, vec![(k, v)])?;
                }
                b'u' => {
                    let items = self.pop_mark()?;
                    let pairs = pairs(items)?;
                    set_items(self.top()?, pairs)?;
                }
                b'c' => {
                    let module = self.line()?;
                    let name = self.line()?;
                    self.stack.push(Value::Global(format!("{module}.{name}")));
                }
                0x93 => {
                    let name = self.pop()?;
                    let module = self.pop()?;
                    match (module, name) {
                        (Value::Str(m), Value::Str(n)) => {
                            self.stack.push(Value::Global(format!("{m}.{n}")));
                        }
                        _ => return Err("STACK_GLOBAL without names".into()),
                    }
                }
                b'R' => {
                    let args = self.pop()?;
                    let callable = self.pop()?;
                    let v = reduce(callable, args)?;
                    self.stack.push(v);
                }
                0x81 => {
                    let args = self.pop()?;
                    let class = self.pop()?;
                    let v = reduce(class, args)?;
                    self.stack.push(v);
                }
                0x92 => {
                    let _kwargs = self.pop()?;
                    let args = self.pop()?;
                    let class = self.pop()?;
                    let v = reduce(class, args)?;
                    self.stack.push(v);
                }
                b'b' => {
                    let state = self.pop()?;
                    // An OrderedDict's attributes (torch's `_metadata`) and
                    // the like are left out: data enough without them.
                    if let Value::Object(o) = self.top()? {
                        o.borrow_mut().state = Some(state);
                    }
                }
                b'Q' => {
                    let pid = self.pop()?;
                    let v = persistent(pid)?;
                    self.stack.push(v);
                }
                b'P' => {
                    let _ = self.line()?;
                    return Err("text persistent ids are not read".into());
                }
                b'q' | b'r' => {
                    let k = if op == b'q' {
                        u64::from(self.byte()?)
                    } else {
                        self.u32()?
                    };
                    let v = self.top()?.clone();
                    self.memo.insert(k, v);
                }
                0x94 => {
                    let k = self.memo.len() as u64;
                    let v = self.top()?.clone();
                    self.memo.insert(k, v);
                }
                b'p' => {
                    let k = self
                        .line()?
                        .trim()
                        .parse::<u64>()
                        .map_err(|e| e.to_string())?;
                    let v = self.top()?.clone();
                    self.memo.insert(k, v);
                }
                b'h' | b'j' | b'g' => {
                    let k = match op {
                        b'h' => u64::from(self.byte()?),
                        b'j' => self.u32()?,
                        _ => self
                            .line()?
                            .trim()
                            .parse::<u64>()
                            .map_err(|e| e.to_string())?,
                    };
                    let v = self
                        .memo
                        .get(&k)
                        .cloned()
                        .ok_or_else(|| format!("memo {k} missing"))?;
                    self.stack.push(v);
                }
                other => return Err(format!("pickle opcode 0x{other:02x} is not read")),
            }
        }
    }
}

fn pairs(items: Vec<Value>) -> Result<Vec<(Value, Value)>, String> {
    if !items.len().is_multiple_of(2) {
        return Err("a dictionary with a key and no value".into());
    }
    let mut it = items.into_iter();
    let mut out = Vec::new();
    while let (Some(k), Some(v)) = (it.next(), it.next()) {
        out.push((k, v));
    }
    Ok(out)
}

fn set_items(target: &mut Value, items: Vec<(Value, Value)>) -> Result<(), String> {
    match target {
        Value::Dict(d) => {
            let mut d = d.borrow_mut();
            for (k, v) in items {
                let same = |e: &&mut (Value, Value)| match (&e.0, &k) {
                    (Value::Str(a), Value::Str(b)) => a == b,
                    (Value::Int(a), Value::Int(b)) => a == b,
                    _ => false,
                };
                match d.iter_mut().find(same) {
                    Some(e) => e.1 = v,
                    None => d.push((k, v)),
                }
            }
            Ok(())
        }
        // A class's items (an object made by REDUCE): kept as its state.
        Value::Object(o) => {
            o.borrow_mut().state = Some(Value::Dict(Rc::new(RefCell::new(items))));
            Ok(())
        }
        _ => Err("SETITEM on a non-dictionary".into()),
    }
}

fn int_items(v: &Value) -> Result<Vec<usize>, String> {
    v.items()
        .ok_or("a tensor's size is not a tuple")?
        .iter()
        .map(|x| {
            x.as_int()
                .and_then(|i| usize::try_from(i).ok())
                .ok_or_else(|| "a tensor's size is not a number".to_string())
        })
        .collect()
}

/// What a call would make: tensors and the few plain values torch's
/// pickles build through calls; any other call a named object.
fn reduce(callable: Value, args: Value) -> Result<Value, String> {
    let Value::Global(name) = &callable else {
        return Err("a call of something not named".into());
    };
    let items = args.items().unwrap_or_default();
    Ok(match name.as_str() {
        "collections.OrderedDict" | "builtins.dict" => {
            Value::Dict(Rc::new(RefCell::new(Vec::new())))
        }
        "builtins.set" | "builtins.frozenset" | "builtins.list" => {
            let first = items.first().and_then(Value::items).unwrap_or_default();
            Value::List(Rc::new(RefCell::new(first)))
        }
        "torch._utils._rebuild_tensor_v2" | "torch._utils._rebuild_tensor" => {
            let (Some(Value::Storage { dtype, key }), Some(offset), Some(size), Some(stride)) =
                (items.first(), items.get(1), items.get(2), items.get(3))
            else {
                return Err("a tensor without its storage".into());
            };
            Value::Tensor(Tensor {
                dtype: *dtype,
                storage: key.clone(),
                offset: offset
                    .as_int()
                    .and_then(|o| usize::try_from(o).ok())
                    .ok_or("a tensor's offset")?,
                shape: int_items(size)?,
                stride: int_items(stride)?,
            })
        }
        "torch._utils._rebuild_parameter" | "torch._utils._rebuild_parameter_with_state" => items
            .into_iter()
            .next()
            .ok_or("a parameter without its data")?,
        "_codecs.encode" => match (items.first(), items.get(1)) {
            (Some(Value::Str(s)), Some(Value::Str(enc))) if enc == "latin1" || enc == "latin-1" => {
                Value::Bytes(s.chars().map(|c| c as u32 as u8).collect())
            }
            (Some(Value::Str(s)), _) => Value::Bytes(s.as_bytes().to_vec()),
            _ => return Err("_codecs.encode without a string".into()),
        },
        "numpy.core.multiarray.scalar" | "numpy._core.multiarray.scalar" => {
            numpy_scalar(&items).unwrap_or_else(|| object(name, args.clone()))
        }
        _ => object(name, args),
    })
}

fn object(name: &str, args: Value) -> Value {
    Value::Object(Rc::new(RefCell::new(Object {
        class: name.to_string(),
        args,
        state: None,
    })))
}

/// A numpy scalar (`scalar(dtype(code), bytes)`) as a number.
fn numpy_scalar(items: &[Value]) -> Option<Value> {
    let (Value::Object(dtype), Value::Bytes(b)) = (items.first()?, items.get(1)?) else {
        return None;
    };
    let dtype = dtype.borrow();
    let code = dtype.args.items()?.first()?.as_str()?.to_string();
    let le = |n: usize| -> Option<[u8; 8]> {
        let mut a = [0u8; 8];
        a[..n].copy_from_slice(b.get(..n)?);
        Some(a)
    };
    Some(match code.trim_start_matches(['<', '=', '|']) {
        "i8" => Value::Int(i64::from_le_bytes(le(8)?)),
        "i4" => Value::Int(i64::from(i32::from_le_bytes(le(4)?[..4].try_into().ok()?))),
        "f8" => Value::Float(f64::from_le_bytes(le(8)?)),
        "f4" => Value::Float(f64::from(f32::from_le_bytes(le(4)?[..4].try_into().ok()?))),
        "b1" => Value::Bool(*b.first()? != 0),
        _ => return None,
    })
}

/// A persistent id: `('storage', <storage class>, key, location, numel)`.
fn persistent(pid: Value) -> Result<Value, String> {
    let items = pid.items().ok_or("a persistent id that is not a tuple")?;
    match (items.first(), items.get(1), items.get(2)) {
        (Some(Value::Str(kind)), Some(Value::Global(class)), Some(key)) if kind == "storage" => {
            let short = class.rsplit('.').next().unwrap_or(class);
            let dtype =
                DType::from_storage(short).ok_or_else(|| format!("storage class {class}"))?;
            let key = match key {
                Value::Str(s) => s.clone(),
                Value::Int(i) => i.to_string(),
                _ => return Err("a storage key".into()),
            };
            Ok(Value::Storage { dtype, key })
        }
        _ => Err("a persistent id other than a storage".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_small_pickle_builds_its_values() {
        // {'a': [1, -2, 3.5], 'b': ('x', True, None), 'c': 300}
        // written by Python's pickle at protocol 2.
        let data: &[u8] = b"\x80\x02}q\x00(X\x01\x00\x00\x00aq\x01]q\x02(K\x01J\xfe\xff\xff\xffG@\x0c\x00\x00\x00\x00\x00\x00eX\x01\x00\x00\x00bq\x03X\x01\x00\x00\x00xq\x04\x88N\x87q\x05X\x01\x00\x00\x00cq\x06M,\x01u.";
        let v = Machine::new(data).run().unwrap();
        let a = v.get("a").unwrap().items().unwrap();
        assert_eq!(a[0].as_int(), Some(1));
        assert_eq!(a[1].as_int(), Some(-2));
        assert!(matches!(a[2], Value::Float(f) if f == 3.5));
        let b = v.get("b").unwrap().items().unwrap();
        assert_eq!(b[0].as_str(), Some("x"));
        assert_eq!(b[1].as_bool(), Some(true));
        assert!(matches!(b[2], Value::None));
        assert_eq!(v.get("c").unwrap().as_int(), Some(300));
    }

    #[test]
    fn long_integers_keep_their_sign() {
        // LONG1 of -129 (two bytes) and of 2**40.
        let data: &[u8] = b"\x80\x02\x8a\x02\x7f\xff\x8a\x06\x00\x00\x00\x00\x00\x01\x86.";
        let v = Machine::new(data).run().unwrap().items().unwrap();
        assert_eq!(v[0].as_int(), Some(-129));
        assert_eq!(v[1].as_int(), Some(1 << 40));
    }

    #[test]
    fn nothing_a_pickle_names_is_called() {
        // os.system('true'): an object that remembers the call, no more.
        let data: &[u8] = b"\x80\x02cos\nsystem\nq\x00X\x04\x00\x00\x00trueq\x01\x85q\x02Rq\x03.";
        match Machine::new(data).run().unwrap() {
            Value::Object(o) => assert_eq!(o.borrow().class, "os.system"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn half_floats_convert() {
        assert_eq!(f16_to_f32(0x3c00), 1.0);
        assert_eq!(f16_to_f32(0xc000), -2.0);
        assert!((f16_to_f32(0x0001) - 5.96e-8).abs() < 1e-9);
    }
}
