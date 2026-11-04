//! Reading the object tree out of a compound file.

use super::{Object, Prop, Stream, Value, WeakRef, WeakRefs, decode_string, sf};
use crate::cfb::{Cfb, CfbError, Kind};
use crate::ids::Auid;
use std::io::{Read, Seek};

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error(transparent)]
    Cfb(#[from] CfbError),
    #[error("not an AAF file: {0}")]
    NotAaf(&'static str),
    #[error(
        "an OMF file: OMF is not read — export the timeline from its application as AAF \
         (Media Composer, Pro Tools and Resolve all write it)"
    )]
    Omf,
    #[error("the AAF file is damaged: {0}")]
    Damaged(String),
}

fn damaged(what: impl Into<String>) -> ReadError {
    ReadError::Damaged(what.into())
}

/// An AAF file's objects, read whole (streams stay in the file).
pub struct StoreReader<R: Read + Seek> {
    pub cfb: Cfb<R>,
    /// The referenced properties: property paths weak references name.
    pub table: Vec<Vec<u16>>,
    objects: usize,
}

const MAX_OBJECTS: usize = 2_000_000;

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

impl<R: Read + Seek> StoreReader<R> {
    pub fn open(file: R) -> Result<Self, ReadError> {
        let mut cfb = Cfb::open(file)?;
        let mut table = Vec::new();
        if let Some(t) = cfb.child(0, "referenced properties") {
            let b = cfb.read(t)?;
            if b.first() != Some(&0x4c) {
                return Err(damaged("the referenced properties are big-endian"));
            }
            let mut path = Vec::new();
            let count = u32_at(&b, 3).unwrap_or(0) as usize;
            for k in 0..count {
                let Some(pid) = u16_at(&b, 7 + 2 * k) else {
                    break;
                };
                if pid == 0 {
                    table.push(std::mem::take(&mut path));
                } else {
                    path.push(pid);
                }
            }
        }
        if cfb.child(0, "Header-2").is_none() {
            return Err(ReadError::NotAaf("there is no header"));
        }
        Ok(StoreReader {
            cfb,
            table,
            objects: 0,
        })
    }

    /// The root object and everything under it.
    pub fn read_root(&mut self) -> Result<Object, ReadError> {
        self.read_object(0, 0)
    }

    /// The object stored in directory entry `dir`.
    pub fn read_object(&mut self, dir: usize, depth: usize) -> Result<Object, ReadError> {
        if depth > 200 {
            return Err(damaged("objects nest too deep"));
        }
        self.objects += 1;
        if self.objects > MAX_OBJECTS {
            return Err(damaged("too many objects"));
        }
        let entry = self
            .cfb
            .entry(dir)
            .ok_or_else(|| damaged("an object is missing"))?;
        let class = Auid::from_le(&entry.clsid).unwrap_or_default();
        let mut obj = Object::new(class);
        let Some(p) = self.cfb.child(dir, "properties") else {
            return Ok(obj);
        };
        let b = self.cfb.read(p)?;
        if b.len() < 4 || b[0] != 0x4c {
            return Err(damaged("an object's properties are not little-endian"));
        }
        let count = usize::from(u16_at(&b, 2).unwrap_or(0));
        let mut at = 4 + 6 * count;
        for k in 0..count {
            let (Some(pid), Some(form), Some(size)) = (
                u16_at(&b, 4 + 6 * k),
                u16_at(&b, 6 + 6 * k),
                u16_at(&b, 8 + 6 * k),
            ) else {
                return Err(damaged("a short properties stream"));
            };
            let size = usize::from(size);
            let data = b
                .get(at..at + size)
                .ok_or_else(|| damaged("a short properties stream"))?
                .to_vec();
            at += size;
            let value = self.value(dir, form, data, depth)?;
            obj.props.push(Prop { pid, value });
        }
        Ok(obj)
    }

    fn child_object(&mut self, dir: usize, name: &str, depth: usize) -> Result<Object, ReadError> {
        let c = self
            .cfb
            .child(dir, name)
            .ok_or_else(|| damaged(format!("{name} is missing")))?;
        if self.cfb.entry(c).map(|e| e.kind) == Some(Kind::Stream) {
            return Err(damaged(format!("{name} is not an object")));
        }
        self.read_object(c, depth + 1)
    }

    fn index(&mut self, dir: usize, name: &str) -> Result<Vec<u8>, ReadError> {
        let i = self
            .cfb
            .child(dir, &format!("{name} index"))
            .ok_or_else(|| damaged(format!("{name} has no index")))?;
        Ok(self.cfb.read(i)?)
    }

    fn weak_refs(&mut self, dir: usize, data: &[u8]) -> Result<WeakRefs, ReadError> {
        let name = decode_string(data);
        let b = self.index(dir, &name)?;
        let count = u32_at(&b, 0).unwrap_or(0) as usize;
        let tag = u16_at(&b, 4).unwrap_or(0);
        let key_pid = u16_at(&b, 6).unwrap_or(0);
        let key_size = *b.get(8).unwrap_or(&16);
        let mut keys = Vec::with_capacity(count.min(4096));
        for k in 0..count {
            let a = 9 + k * usize::from(key_size);
            let key = b
                .get(a..a + usize::from(key_size))
                .ok_or_else(|| damaged("a short weak reference index"))?;
            keys.push(key.to_vec());
        }
        Ok(WeakRefs {
            path: self
                .table
                .get(usize::from(tag))
                .cloned()
                .unwrap_or_default(),
            key_pid,
            key_size,
            keys,
        })
    }

    fn value(
        &mut self,
        dir: usize,
        form: u16,
        data: Vec<u8>,
        depth: usize,
    ) -> Result<Value, ReadError> {
        Ok(match form {
            sf::DATA_STREAM => {
                let name = decode_string(data.get(1..).unwrap_or(&[]));
                match self.cfb.child(dir, &name) {
                    Some(e) => Value::Stream(Stream::InFile {
                        entry: e,
                        len: self.cfb.entry(e).map_or(0, |e| e.size),
                    }),
                    None => Value::Stream(Stream::Bytes(Vec::new())),
                }
            }
            sf::STRONG => {
                let name = decode_string(&data);
                Value::Strong(Box::new(self.child_object(dir, &name, depth)?))
            }
            sf::STRONG_VECTOR => {
                let name = decode_string(&data);
                let b = self.index(dir, &name)?;
                let count = u32_at(&b, 0).unwrap_or(0) as usize;
                let mut items = Vec::with_capacity(count.min(4096));
                for k in 0..count {
                    let key =
                        u32_at(&b, 12 + 4 * k).ok_or_else(|| damaged("a short vector index"))?;
                    items.push(self.child_object(dir, &format!("{name}{{{key:x}}}"), depth)?);
                }
                Value::StrongVector(items)
            }
            sf::STRONG_SET => {
                let name = decode_string(&data);
                let b = self.index(dir, &name)?;
                let count = u32_at(&b, 0).unwrap_or(0) as usize;
                let key_pid = u16_at(&b, 12).unwrap_or(0);
                let key_size = *b.get(14).unwrap_or(&16);
                let step = 8 + usize::from(key_size);
                let mut items = Vec::with_capacity(count.min(4096));
                for k in 0..count {
                    let local =
                        u32_at(&b, 15 + k * step).ok_or_else(|| damaged("a short set index"))?;
                    items.push(self.child_object(dir, &format!("{name}{{{local:x}}}"), depth)?);
                }
                Value::StrongSet {
                    key_pid,
                    key_from: key_pid,
                    key_size,
                    items,
                }
            }
            sf::WEAK => {
                let tag = u16_at(&data, 0).unwrap_or(0);
                let key_pid = u16_at(&data, 2).unwrap_or(0);
                let size = usize::from(*data.get(4).unwrap_or(&0));
                Value::Weak(WeakRef {
                    path: self
                        .table
                        .get(usize::from(tag))
                        .cloned()
                        .unwrap_or_default(),
                    key_pid,
                    key: data.get(5..5 + size).unwrap_or(&[]).to_vec(),
                })
            }
            sf::WEAK_VECTOR => Value::WeakVector(self.weak_refs(dir, &data)?),
            sf::WEAK_SET => Value::WeakSet(self.weak_refs(dir, &data)?),
            _ => Value::Data(data),
        })
    }
}
