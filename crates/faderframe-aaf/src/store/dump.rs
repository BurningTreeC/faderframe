//! A readable outline of an object tree (golden tests, diagnostics):
//! classes and properties by name, values short.

use super::{Object, Stream, Value, decode_string};
use crate::ids::Auid;
use crate::model::{self, TypeKind};
use std::fmt::Write;

fn prop_name(class: Auid, pid: u16) -> (String, Option<Auid>) {
    let mut c = model::class(class);
    while let Some(d) = c {
        if let Some(p) = d.props.iter().find(|p| p.pid == pid) {
            return (p.name.to_string(), Some(p.ty));
        }
        if d.parent == d.id {
            break;
        }
        c = model::class(d.parent);
    }
    (format!("{pid:#06x}"), None)
}

fn data_text(b: &[u8], ty: Option<Auid>) -> String {
    let kind = ty.and_then(model::type_def).map(|t| (&t.kind, t.name));
    match kind {
        Some((TypeKind::String { .. }, _)) => format!("{:?}", decode_string(b)),
        Some((TypeKind::Int { .. } | TypeKind::Rename { .. } | TypeKind::Enum { .. }, _))
            if b.len() <= 8 =>
        {
            let mut raw = [0u8; 8];
            raw[..b.len()].copy_from_slice(b);
            let v = u64::from_le_bytes(raw);
            // Signed when the top bit of its size is set.
            if b.len() == 8 && (v as i64) < 0 {
                format!("{}", v as i64)
            } else {
                format!("{v}")
            }
        }
        Some((_, "Rational")) if b.len() == 8 => {
            let r = super::Rational::decode(b).unwrap_or(super::Rational::new(0, 0));
            format!("{}/{}", r.num, r.den)
        }
        Some((_, "AUID")) | Some((TypeKind::ExtEnum { .. }, _)) => {
            Auid::from_le(b).map_or_else(String::new, |a| a.to_string())
        }
        Some((_, "MobIDType")) => "mob".into(),
        _ => format!("{} bytes", b.len()),
    }
}

/// The outline of `o`, `indent` deep.
pub fn dump(o: &Object, indent: usize, out: &mut String) {
    let class = model::class(o.class).map_or_else(|| o.class.to_string(), |c| c.name.into());
    let _ = writeln!(out, "{:indent$}{class}", "", indent = indent * 2);
    for p in &o.props {
        let (name, ty) = prop_name(o.class, p.pid);
        let pad = (indent + 1) * 2;
        match &p.value {
            Value::Data(b) => {
                let _ = writeln!(out, "{:pad$}{name} = {}", "", data_text(b, ty));
            }
            Value::Stream(s) => {
                let len = match s {
                    Stream::Bytes(b) => b.len() as u64,
                    Stream::Pending { len, .. } | Stream::InFile { len, .. } => *len,
                };
                let _ = writeln!(out, "{:pad$}{name} = stream of {len} bytes", "");
            }
            Value::Strong(c) => {
                let _ = writeln!(out, "{:pad$}{name}:", "");
                dump(c, indent + 2, out);
            }
            Value::StrongVector(items) | Value::StrongSet { items, .. } => {
                let _ = writeln!(out, "{:pad$}{name}: {} items", "", items.len());
                for c in items {
                    dump(c, indent + 2, out);
                }
            }
            Value::Weak(w) => {
                let key = Auid::from_le(&w.key).map_or_else(|| "mob".into(), |a| a.to_string());
                let _ = writeln!(out, "{:pad$}{name} -> {key}", "");
            }
            Value::WeakVector(w) | Value::WeakSet(w) => {
                let _ = writeln!(out, "{:pad$}{name} -> {} references", "", w.keys.len());
            }
        }
    }
}
