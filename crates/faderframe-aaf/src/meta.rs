//! The meta dictionary a file carries: every class of the model with its
//! properties, every type, as objects (ClassDefinition, PropertyDefinition,
//! TypeDefinition…). Written whole, like the AAF SDK and pyaaf2 do, so a
//! reader needs nothing of its own to know any property in the file.

use crate::ids::Auid;
use crate::model::{self, ClassDef, TypeDef, TypeKind, cls, pid, ty};
use crate::store::{Object, Value, WeakRefs};

fn set(items: Vec<Object>) -> Value {
    Value::StrongSet {
        key_pid: pid::META_IDENTIFICATION,
        key_from: pid::META_IDENTIFICATION,
        key_size: 16,
        items,
    }
}

fn strings(items: impl IntoIterator<Item = &'static str>) -> Value {
    let mut b = Vec::new();
    for s in items {
        b.extend_from_slice(&crate::store::encode_string(s));
    }
    Value::Data(b)
}

fn auids(items: impl IntoIterator<Item = Auid>) -> Value {
    let mut b = Vec::new();
    for a in items {
        b.extend_from_slice(&a.to_le());
    }
    Value::Data(b)
}

fn class_object(c: &ClassDef) -> Object {
    let props = c
        .props
        .iter()
        .map(|p| {
            Object::new(cls::PROPERTY_DEFINITION)
                .with(pid::META_NAME, Value::string(p.name))
                .with(pid::PROPERTY_IS_OPTIONAL, Value::bool(p.optional))
                .with(pid::PROPERTY_IS_UNIQUE, Value::bool(p.unique))
                .with(pid::PROPERTY_LOCAL_ID, Value::u16(p.pid))
                .with(pid::META_IDENTIFICATION, Value::auid(p.id))
                .with(pid::PROPERTY_TYPE, Value::auid(p.ty))
        })
        .collect();
    Object::new(cls::CLASS_DEFINITION)
        .with(pid::META_NAME, Value::string(c.name))
        .with(pid::META_IDENTIFICATION, Value::auid(c.id))
        .with(pid::CLASS_IS_CONCRETE, Value::bool(c.concrete))
        .with(
            pid::CLASS_PARENT,
            Value::weak(ty::CLASS_DEFINITION_WEAK, c.parent),
        )
        .with(pid::CLASS_PROPERTIES, set(props))
}

fn type_object(t: &TypeDef) -> Object {
    let type_ref = |a: Auid| Value::weak(ty::TYPE_DEFINITION_WEAK, a);
    let class = match &t.kind {
        TypeKind::Int { .. } => cls::TYPE_DEF_INT,
        TypeKind::Enum { .. } => cls::TYPE_DEF_ENUM,
        TypeKind::Record { .. } => cls::TYPE_DEF_RECORD,
        TypeKind::FixedArray { .. } => cls::TYPE_DEF_FIXED_ARRAY,
        TypeKind::VarArray { .. } => cls::TYPE_DEF_VAR_ARRAY,
        TypeKind::Set { .. } => cls::TYPE_DEF_SET,
        TypeKind::Rename { .. } => cls::TYPE_DEF_RENAME,
        TypeKind::String { .. } => cls::TYPE_DEF_STRING,
        TypeKind::Stream => cls::TYPE_DEF_STREAM,
        TypeKind::Opaque => cls::TYPE_DEF_OPAQUE,
        TypeKind::Indirect => cls::TYPE_DEF_INDIRECT,
        TypeKind::ExtEnum { .. } => cls::TYPE_DEF_EXT_ENUM,
        TypeKind::Character => cls::TYPE_DEF_CHARACTER,
        TypeKind::GenericCharacter { .. } => cls::TYPE_DEF_GENERIC_CHARACTER,
        TypeKind::StrongRef { .. } => cls::TYPE_DEF_STRONG_REF,
        TypeKind::WeakRef { .. } => cls::TYPE_DEF_WEAK_REF,
    };
    let mut o = Object::new(class)
        .with(pid::META_NAME, Value::string(t.name))
        .with(pid::META_IDENTIFICATION, Value::auid(t.id));
    match &t.kind {
        TypeKind::Int { size, signed } => {
            o.set(pid::INT_SIZE, Value::u8(*size));
            o.set(pid::INT_SIGNED, Value::bool(*signed));
        }
        TypeKind::Enum { element, items } => {
            o.set(pid::ENUM_TYPE, type_ref(*element));
            o.set(pid::ENUM_NAMES, strings(items.iter().map(|i| i.0)));
            let mut b = Vec::new();
            for (_, v) in items.iter() {
                b.extend_from_slice(&v.to_le_bytes());
            }
            o.set(pid::ENUM_VALUES, Value::Data(b));
        }
        TypeKind::Record { members } => {
            o.set(pid::RECORD_NAMES, strings(members.iter().map(|m| m.0)));
            let types: Vec<Auid> = members.iter().map(|m| m.1).collect();
            let refs: WeakRefs = Value::weak_refs(ty::TYPE_DEFINITION_WEAK, &types);
            o.set(pid::RECORD_TYPES, Value::WeakVector(refs));
        }
        TypeKind::FixedArray { element, count } => {
            o.set(pid::FIXED_TYPE, type_ref(*element));
            o.set(pid::FIXED_COUNT, Value::u32(*count));
        }
        TypeKind::VarArray { element } => o.set(pid::VAR_TYPE, type_ref(*element)),
        TypeKind::Set { element } => o.set(pid::SET_TYPE, type_ref(*element)),
        TypeKind::Rename { of } => o.set(pid::RENAME_TYPE, type_ref(*of)),
        TypeKind::String { element } => o.set(pid::STRING_TYPE, type_ref(*element)),
        TypeKind::ExtEnum { items } => {
            o.set(pid::EXT_ENUM_NAMES, strings(items.iter().map(|i| i.0)));
            o.set(pid::EXT_ENUM_VALUES, auids(items.iter().map(|i| i.1)));
        }
        TypeKind::GenericCharacter { size, pid } => o.set(*pid, Value::u8(*size)),
        TypeKind::StrongRef { class } => {
            o.set(
                pid::STRONG_REF_TYPE,
                Value::weak(ty::CLASS_DEFINITION_WEAK, *class),
            );
        }
        TypeKind::WeakRef { class, path } => {
            o.set(
                pid::WEAK_REF_TYPE,
                Value::weak(ty::CLASS_DEFINITION_WEAK, *class),
            );
            o.set(pid::WEAK_TARGET_SET, auids(path.iter().copied()));
        }
        TypeKind::Stream | TypeKind::Opaque | TypeKind::Indirect | TypeKind::Character => {}
    }
    o
}

/// The meta dictionary of the whole model.
pub fn meta_dictionary() -> Object {
    let classes = model::CLASSES
        .iter()
        .filter(|c| c.id != cls::ROOT)
        .map(class_object)
        .collect();
    let types = model::TYPES.iter().map(type_object).collect();
    Object::new(cls::META_DICTIONARY)
        .with(pid::META_CLASS_DEFINITIONS, set(classes))
        .with(pid::META_TYPE_DEFINITIONS, set(types))
}
