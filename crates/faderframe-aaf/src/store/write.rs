//! Writing an object tree into a compound file.

use super::{Object, Stream, Value, WeakRefs, mangle, sf};
use crate::cfb::{CfbError, CfbWriter, EntryId};
use crate::model;
use std::io::{self, Seek, Write};

/// Fills [`Stream::Pending`] streams as they are written: the token and
/// where to write the stream's bytes.
pub type StreamSource<'a> = dyn FnMut(usize, &mut dyn Write) -> io::Result<()> + 'a;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Cfb(#[from] CfbError),
    #[error("property {pid:#06x} of a {class} holds {len} bytes, more than a property can")]
    TooLarge { class: String, pid: u16, len: usize },
    #[error("objects nest too deep")]
    TooDeep,
}

/// Writes objects, keeping the table of the property paths weak
/// references point into.
pub struct StoreWriter<'s, W: Write + Seek> {
    pub(crate) cfb: CfbWriter<W>,
    table: Vec<Vec<u16>>,
    streams: &'s mut StreamSource<'s>,
}

/// A property's name for its directory entries.
fn prop_name(class: crate::ids::Auid, pid: u16) -> &'static str {
    let mut c = model::class(class);
    while let Some(d) = c {
        if let Some(p) = d.props.iter().find(|p| p.pid == pid) {
            return p.name;
        }
        if d.parent == d.id {
            break;
        }
        c = model::class(d.parent);
    }
    "Property"
}

impl<'s, W: Write + Seek> StoreWriter<'s, W> {
    pub fn new(cfb: CfbWriter<W>, streams: &'s mut StreamSource<'s>) -> Self {
        StoreWriter {
            cfb,
            table: Vec::new(),
            streams,
        }
    }

    fn tag(&mut self, path: &[u16]) -> u16 {
        match self.table.iter().position(|p| p == path) {
            Some(i) => i as u16,
            None => {
                self.table.push(path.to_vec());
                (self.table.len() - 1) as u16
            }
        }
    }

    /// The root object (its storage is the file's root), and the table of
    /// referenced properties after it.
    pub fn write_root(mut self, root: &Object) -> Result<W, StoreError> {
        let r = self.cfb.root();
        self.write_object(root, r, 0)?;
        let mut b = vec![0x4c];
        b.extend_from_slice(&(self.table.len() as u16).to_le_bytes());
        let count: usize = self.table.iter().map(|p| p.len() + 1).sum();
        b.extend_from_slice(&(count as u32).to_le_bytes());
        for p in &self.table {
            for pid in p {
                b.extend_from_slice(&pid.to_le_bytes());
            }
            b.extend_from_slice(&0u16.to_le_bytes());
        }
        self.cfb.stream(r, "referenced properties", &b)?;
        Ok(self.cfb.finish()?)
    }

    fn weak_index(&mut self, dir: EntryId, name: &str, refs: &WeakRefs) -> Result<(), StoreError> {
        let tag = self.tag(&refs.path);
        let mut b = (refs.keys.len() as u32).to_le_bytes().to_vec();
        b.extend_from_slice(&tag.to_le_bytes());
        b.extend_from_slice(&refs.key_pid.to_le_bytes());
        b.push(refs.key_size);
        for k in &refs.keys {
            b.extend_from_slice(k);
        }
        self.cfb.stream(dir, &format!("{name} index"), &b)?;
        Ok(())
    }

    fn write_object(&mut self, obj: &Object, dir: EntryId, depth: usize) -> Result<(), StoreError> {
        if depth > 200 {
            return Err(StoreError::TooDeep);
        }
        self.cfb.set_clsid(dir, obj.class.to_le());
        let mut entries: Vec<(u16, u16, Vec<u8>)> = Vec::with_capacity(obj.props.len());
        for p in &obj.props {
            let name = prop_name(obj.class, p.pid);
            let (form, data) = match &p.value {
                Value::Data(d) => (sf::DATA, d.clone()),
                Value::Stream(s) => {
                    let n = mangle(name, p.pid, 32);
                    match s {
                        Stream::Bytes(b) => {
                            self.cfb.stream(dir, &n, b)?;
                        }
                        Stream::Pending { len, token } => {
                            let token = *token;
                            let streams = &mut *self.streams;
                            self.cfb
                                .stream_with(dir, &n, *len, &mut |w| streams(token, w))?;
                        }
                        Stream::InFile { .. } => {
                            self.cfb.stream(dir, &n, &[])?;
                        }
                    }
                    let mut d = vec![0x55];
                    d.extend_from_slice(&super::encode_string(&n));
                    (sf::DATA_STREAM, d)
                }
                Value::Strong(o) => {
                    let n = mangle(name, p.pid, 32);
                    let child = self.cfb.storage(dir, &n, [0; 16])?;
                    self.write_object(o, child, depth + 1)?;
                    (sf::STRONG, super::encode_string(&n))
                }
                Value::StrongVector(items) => {
                    let n = mangle(name, p.pid, 22);
                    for (k, o) in items.iter().enumerate() {
                        let child = self.cfb.storage(dir, &format!("{n}{{{k:x}}}"), [0; 16])?;
                        self.write_object(o, child, depth + 1)?;
                    }
                    let mut b = (items.len() as u32).to_le_bytes().to_vec();
                    b.extend_from_slice(&(items.len() as u32).to_le_bytes());
                    b.extend_from_slice(&u32::MAX.to_le_bytes());
                    for k in 0..items.len() as u32 {
                        b.extend_from_slice(&k.to_le_bytes());
                    }
                    self.cfb.stream(dir, &format!("{n} index"), &b)?;
                    (sf::STRONG_VECTOR, super::encode_string(&n))
                }
                Value::StrongSet {
                    key_pid,
                    key_from,
                    key_size,
                    items,
                } => {
                    let n = mangle(name, p.pid, 22);
                    let mut b = (items.len() as u32).to_le_bytes().to_vec();
                    b.extend_from_slice(&(items.len() as u32).to_le_bytes());
                    b.extend_from_slice(&u32::MAX.to_le_bytes());
                    b.extend_from_slice(&key_pid.to_le_bytes());
                    b.push(*key_size);
                    for (k, o) in items.iter().enumerate() {
                        let child = self.cfb.storage(dir, &format!("{n}{{{k:x}}}"), [0; 16])?;
                        self.write_object(o, child, depth + 1)?;
                        let mut key = o.data(*key_from).unwrap_or(&[]).to_vec();
                        key.resize(usize::from(*key_size), 0);
                        b.extend_from_slice(&(k as u32).to_le_bytes());
                        b.extend_from_slice(&1u32.to_le_bytes());
                        b.extend_from_slice(&key);
                    }
                    self.cfb.stream(dir, &format!("{n} index"), &b)?;
                    (sf::STRONG_SET, super::encode_string(&n))
                }
                Value::Weak(w) => {
                    let tag = self.tag(&w.path);
                    let mut d = tag.to_le_bytes().to_vec();
                    d.extend_from_slice(&w.key_pid.to_le_bytes());
                    d.push(w.key.len() as u8);
                    d.extend_from_slice(&w.key);
                    (sf::WEAK, d)
                }
                Value::WeakVector(refs) | Value::WeakSet(refs) => {
                    let n = mangle(name, p.pid, 22);
                    self.weak_index(dir, &n, refs)?;
                    let form = if matches!(p.value, Value::WeakVector(_)) {
                        sf::WEAK_VECTOR
                    } else {
                        sf::WEAK_SET
                    };
                    (form, super::encode_string(&n))
                }
            };
            if data.len() > usize::from(u16::MAX) {
                return Err(StoreError::TooLarge {
                    class: model::class(obj.class)
                        .map_or_else(|| obj.class.to_string(), |c| c.name.to_string()),
                    pid: p.pid,
                    len: data.len(),
                });
            }
            entries.push((p.pid, form, data));
        }
        let mut b = vec![0x4c, 0x20];
        b.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        for (pid, form, data) in &entries {
            b.extend_from_slice(&pid.to_le_bytes());
            b.extend_from_slice(&form.to_le_bytes());
            b.extend_from_slice(&(data.len() as u16).to_le_bytes());
        }
        for (_, _, data) in &entries {
            b.extend_from_slice(data);
        }
        self.cfb.stream(dir, "properties", &b)?;
        Ok(())
    }
}
