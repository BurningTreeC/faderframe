use super::read::StoreReader;
use super::write::StoreWriter;
use super::*;
use crate::cfb::{CfbWriter, SectorSize};
use crate::model::{cls, pid, ty};
use std::io::Cursor;

#[test]
fn names_are_mangled_like_the_sdk_does() {
    assert_eq!(mangle("Header", 0x2, 32), "Header-2");
    assert_eq!(mangle("Mobs", 0x1901, 22), "Mobs-1901");
    assert_eq!(
        mangle("TranTKMNTrackedParamSetngs", 0xff86, 22),
        "TranTKMN-mSetngs-ff86"
    );
}

#[test]
fn objects_round_trip_through_a_compound_file() {
    let clip = Object::new(cls::SOURCE_CLIP)
        .with(pid::COMPONENT_LENGTH, Value::i64(480_000))
        .with(
            pid::COMPONENT_DATA_DEFINITION,
            Value::weak(ty::DATA_DEFINITION_WEAK, crate::model::defs::DATA_SOUND),
        );
    let seq = Object::new(cls::SEQUENCE).with(
        pid::SEQUENCE_COMPONENTS,
        Value::StrongVector(vec![clip.clone(), clip.clone()]),
    );
    let mob = |n: u8| {
        Object::new(cls::COMPOSITION_MOB)
            .with(pid::MOB_MOB_ID, Value::Data(vec![n; 32]))
            .with(pid::MOB_NAME, Value::string("Mix"))
            .with(pid::SLOT_SEGMENT, Value::Strong(Box::new(seq.clone())))
            .with(
                pid::ESSENCE_DATA_DATA,
                Value::Stream(Stream::Pending {
                    len: 10_000 + u64::from(n),
                    token: usize::from(n),
                }),
            )
    };
    let header = Object::new(cls::HEADER).with(
        pid::CONTENT_MOBS,
        Value::StrongSet {
            key_pid: pid::MOB_MOB_ID,
            key_from: pid::MOB_MOB_ID,
            key_size: 32,
            items: vec![mob(1), mob(2)],
        },
    );
    let root = Object::new(cls::ROOT)
        .with(
            pid::ROOT_META_DICTIONARY,
            Value::Strong(Box::new(Object::new(cls::META_DICTIONARY))),
        )
        .with(pid::ROOT_HEADER, Value::Strong(Box::new(header)));
    let mut fill =
        |token: usize, w: &mut dyn std::io::Write| w.write_all(&vec![token as u8; 10_000 + token]);
    let cfb = CfbWriter::new(Cursor::new(Vec::new()), SectorSize::Large).unwrap();
    let bytes = StoreWriter::new(cfb, &mut fill)
        .write_root(&root)
        .unwrap()
        .into_inner();

    let mut r = StoreReader::open(Cursor::new(bytes)).unwrap();
    assert_eq!(r.table, vec![vec![0x0002, 0x3b04, 0x2605]]);
    let back = r.read_root().unwrap();
    assert_eq!(back.class, cls::ROOT);
    let h = back.strong(pid::ROOT_HEADER).unwrap();
    let mobs = h.objects(pid::CONTENT_MOBS);
    assert_eq!(mobs.len(), 2);
    assert_eq!(mobs[1].string(pid::MOB_NAME).as_deref(), Some("Mix"));
    let seq = mobs[0].strong(pid::SLOT_SEGMENT).unwrap();
    let clips = seq.objects(pid::SEQUENCE_COMPONENTS);
    assert_eq!(clips.len(), 2);
    assert_eq!(clips[1].i64(pid::COMPONENT_LENGTH), Some(480_000));
    assert_eq!(
        clips[0].weak_auid(pid::COMPONENT_DATA_DEFINITION),
        Some(crate::model::defs::DATA_SOUND)
    );
    let Some(Stream::InFile { entry, len }) = mobs[1].stream(pid::ESSENCE_DATA_DATA).cloned()
    else {
        panic!("no stream");
    };
    assert_eq!(len, 10_002);
    assert_eq!(r.cfb.read(entry).unwrap(), vec![2u8; 10_002]);
}
