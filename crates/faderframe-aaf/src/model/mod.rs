//! The AAF object model: every class with its properties (local ids,
//! types, whether they are optional) and every type, as an AAF file's meta
//! dictionary declares them ([`baseline`], generated). [`cls`] and [`pid`]
//! name the classes and properties this crate writes and reads; a test
//! holds them to the tables.

mod baseline;
pub mod defs;

use crate::ids::Auid;

pub(crate) use baseline::{CLASSES, TYPES};

/// A class of the object model.
#[derive(Debug)]
pub struct ClassDef {
    pub name: &'static str,
    pub id: Auid,
    /// Its parent class (itself for InterchangeObject and the root).
    pub parent: Auid,
    pub concrete: bool,
    /// The properties it adds to its parent's.
    pub props: &'static [PropDef],
}

/// A property of a class.
#[derive(Debug)]
pub struct PropDef {
    pub name: &'static str,
    pub id: Auid,
    /// Its local id in files (dynamic ones, 0x8000 and up, as pyaaf2 and
    /// the AAF SDK give them).
    pub pid: u16,
    pub ty: Auid,
    pub optional: bool,
    pub unique: bool,
}

/// A type of the object model.
#[derive(Debug)]
pub struct TypeDef {
    pub name: &'static str,
    pub id: Auid,
    pub kind: TypeKind,
}

#[derive(Debug)]
pub enum TypeKind {
    Int {
        size: u8,
        signed: bool,
    },
    Enum {
        element: Auid,
        items: &'static [(&'static str, i64)],
    },
    Record {
        members: &'static [(&'static str, Auid)],
    },
    FixedArray {
        element: Auid,
        count: u32,
    },
    VarArray {
        element: Auid,
    },
    Set {
        element: Auid,
    },
    Rename {
        of: Auid,
    },
    String {
        element: Auid,
    },
    Stream,
    Opaque,
    Indirect,
    ExtEnum {
        items: &'static [(&'static str, Auid)],
    },
    Character,
    GenericCharacter {
        size: u8,
        pid: u16,
    },
    StrongRef {
        class: Auid,
    },
    WeakRef {
        class: Auid,
        /// The property path from the root to the set the reference
        /// points into.
        path: &'static [Auid],
    },
}

/// The class `id`.
pub fn class(id: Auid) -> Option<&'static ClassDef> {
    CLASSES.iter().find(|c| c.id == id)
}

/// The class called `name`.
pub fn class_named(name: &str) -> Option<&'static ClassDef> {
    CLASSES.iter().find(|c| c.name == name)
}

/// The type `id`.
pub fn type_def(id: Auid) -> Option<&'static TypeDef> {
    TYPES.iter().find(|t| t.id == id)
}

/// A property of `class` or of one of its ancestors, by name.
pub fn property(class: Auid, name: &str) -> Option<&'static PropDef> {
    let mut c = self::class(class)?;
    loop {
        if let Some(p) = c.props.iter().find(|p| p.name == name) {
            return Some(p);
        }
        if c.parent == c.id {
            return None;
        }
        c = self::class(c.parent)?;
    }
}

/// Whether `class` is `ancestor` or derives from it (baseline classes).
pub fn is_a(class: Auid, ancestor: Auid) -> bool {
    let mut c = class;
    for _ in 0..32 {
        if c == ancestor {
            return true;
        }
        match self::class(c) {
            Some(d) if d.parent != d.id => c = d.parent,
            _ => return false,
        }
    }
    false
}

/// The class a property's objects are (strong references and sets or
/// vectors of them).
fn referenced_class(ty: Auid) -> Option<Auid> {
    match &type_def(ty)?.kind {
        TypeKind::StrongRef { class } | TypeKind::WeakRef { class, .. } => Some(*class),
        TypeKind::Set { element } | TypeKind::VarArray { element } => referenced_class(*element),
        _ => None,
    }
}

/// The local ids from the root to the set a weak reference type points
/// into (the "referenced properties" a file lists).
pub fn weak_path(ty: Auid) -> Option<Vec<u16>> {
    let path = match &type_def(ty)?.kind {
        TypeKind::WeakRef { path, .. } => *path,
        TypeKind::Set { element } | TypeKind::VarArray { element } => return weak_path(*element),
        _ => return None,
    };
    let mut class = cls::ROOT;
    let mut pids = Vec::with_capacity(path.len());
    for step in path {
        let mut c = self::class(class)?;
        let p = loop {
            if let Some(p) = c.props.iter().find(|p| p.id == *step) {
                break p;
            }
            if c.parent == c.id {
                return None;
            }
            c = self::class(c.parent)?;
        };
        pids.push(p.pid);
        class = referenced_class(p.ty)?;
    }
    Some(pids)
}

/// The class ids this crate uses.
pub mod cls {
    use crate::ids::{Auid, id};

    pub const ROOT: Auid = id(0xb3b398a5_1c90_11d4_8053_080036210804);
    pub const META_DICTIONARY: Auid = id(0x0d010101_0225_0000_060e_2b3402060101);
    pub const CLASS_DEFINITION: Auid = id(0x0d010101_0201_0000_060e_2b3402060101);
    pub const PROPERTY_DEFINITION: Auid = id(0x0d010101_0202_0000_060e_2b3402060101);
    pub const TYPE_DEF_INT: Auid = id(0x0d010101_0204_0000_060e_2b3402060101);
    pub const TYPE_DEF_STRONG_REF: Auid = id(0x0d010101_0205_0000_060e_2b3402060101);
    pub const TYPE_DEF_WEAK_REF: Auid = id(0x0d010101_0206_0000_060e_2b3402060101);
    pub const TYPE_DEF_ENUM: Auid = id(0x0d010101_0207_0000_060e_2b3402060101);
    pub const TYPE_DEF_FIXED_ARRAY: Auid = id(0x0d010101_0208_0000_060e_2b3402060101);
    pub const TYPE_DEF_VAR_ARRAY: Auid = id(0x0d010101_0209_0000_060e_2b3402060101);
    pub const TYPE_DEF_SET: Auid = id(0x0d010101_020a_0000_060e_2b3402060101);
    pub const TYPE_DEF_STRING: Auid = id(0x0d010101_020b_0000_060e_2b3402060101);
    pub const TYPE_DEF_STREAM: Auid = id(0x0d010101_020c_0000_060e_2b3402060101);
    pub const TYPE_DEF_RECORD: Auid = id(0x0d010101_020d_0000_060e_2b3402060101);
    pub const TYPE_DEF_RENAME: Auid = id(0x0d010101_020e_0000_060e_2b3402060101);
    pub const TYPE_DEF_EXT_ENUM: Auid = id(0x0d010101_0220_0000_060e_2b3402060101);
    pub const TYPE_DEF_INDIRECT: Auid = id(0x0d010101_0221_0000_060e_2b3402060101);
    pub const TYPE_DEF_OPAQUE: Auid = id(0x0d010101_0222_0000_060e_2b3402060101);
    pub const TYPE_DEF_CHARACTER: Auid = id(0x0d010101_0223_0000_060e_2b3402060101);
    pub const TYPE_DEF_GENERIC_CHARACTER: Auid = id(0x0e040101_0000_0000_060e_2b3402060101);

    pub const COMPONENT: Auid = id(0x0d010101_0101_0200_060e_2b3402060101);
    pub const SEGMENT: Auid = id(0x0d010101_0101_0300_060e_2b3402060101);
    pub const ESSENCE_GROUP: Auid = id(0x0d010101_0101_0500_060e_2b3402060101);
    pub const EVENT: Auid = id(0x0d010101_0101_0600_060e_2b3402060101);
    pub const COMMENT_MARKER: Auid = id(0x0d010101_0101_0800_060e_2b3402060101);
    pub const FILLER: Auid = id(0x0d010101_0101_0900_060e_2b3402060101);
    pub const OPERATION_GROUP: Auid = id(0x0d010101_0101_0a00_060e_2b3402060101);
    pub const NESTED_SCOPE: Auid = id(0x0d010101_0101_0b00_060e_2b3402060101);
    pub const SCOPE_REFERENCE: Auid = id(0x0d010101_0101_0d00_060e_2b3402060101);
    pub const SELECTOR: Auid = id(0x0d010101_0101_0e00_060e_2b3402060101);
    pub const SEQUENCE: Auid = id(0x0d010101_0101_0f00_060e_2b3402060101);
    pub const SOURCE_CLIP: Auid = id(0x0d010101_0101_1100_060e_2b3402060101);
    pub const TIMECODE: Auid = id(0x0d010101_0101_1400_060e_2b3402060101);
    pub const TRANSITION: Auid = id(0x0d010101_0101_1700_060e_2b3402060101);
    pub const CONTENT_STORAGE: Auid = id(0x0d010101_0101_1800_060e_2b3402060101);
    pub const CONTROL_POINT: Auid = id(0x0d010101_0101_1900_060e_2b3402060101);
    pub const DATA_DEFINITION: Auid = id(0x0d010101_0101_1b00_060e_2b3402060101);
    pub const OPERATION_DEFINITION: Auid = id(0x0d010101_0101_1c00_060e_2b3402060101);
    pub const PARAMETER_DEFINITION: Auid = id(0x0d010101_0101_1d00_060e_2b3402060101);
    pub const CODEC_DEFINITION: Auid = id(0x0d010101_0101_1f00_060e_2b3402060101);
    pub const CONTAINER_DEFINITION: Auid = id(0x0d010101_0101_2000_060e_2b3402060101);
    pub const INTERPOLATION_DEFINITION: Auid = id(0x0d010101_0101_2100_060e_2b3402060101);
    pub const DICTIONARY: Auid = id(0x0d010101_0101_2200_060e_2b3402060101);
    pub const ESSENCE_DATA: Auid = id(0x0d010101_0101_2300_060e_2b3402060101);
    pub const ESSENCE_DESCRIPTOR: Auid = id(0x0d010101_0101_2400_060e_2b3402060101);
    pub const FILE_DESCRIPTOR: Auid = id(0x0d010101_0101_2500_060e_2b3402060101);
    pub const AIFC_DESCRIPTOR: Auid = id(0x0d010101_0101_2600_060e_2b3402060101);
    pub const WAVE_DESCRIPTOR: Auid = id(0x0d010101_0101_2c00_060e_2b3402060101);
    pub const TAPE_DESCRIPTOR: Auid = id(0x0d010101_0101_2e00_060e_2b3402060101);
    pub const HEADER: Auid = id(0x0d010101_0101_2f00_060e_2b3402060101);
    pub const IDENTIFICATION: Auid = id(0x0d010101_0101_3000_060e_2b3402060101);
    pub const NETWORK_LOCATOR: Auid = id(0x0d010101_0101_3200_060e_2b3402060101);
    pub const MOB: Auid = id(0x0d010101_0101_3400_060e_2b3402060101);
    pub const COMPOSITION_MOB: Auid = id(0x0d010101_0101_3500_060e_2b3402060101);
    pub const MASTER_MOB: Auid = id(0x0d010101_0101_3600_060e_2b3402060101);
    pub const SOURCE_MOB: Auid = id(0x0d010101_0101_3700_060e_2b3402060101);
    pub const MOB_SLOT: Auid = id(0x0d010101_0101_3800_060e_2b3402060101);
    pub const EVENT_MOB_SLOT: Auid = id(0x0d010101_0101_3900_060e_2b3402060101);
    pub const TIMELINE_MOB_SLOT: Auid = id(0x0d010101_0101_3b00_060e_2b3402060101);
    pub const PARAMETER: Auid = id(0x0d010101_0101_3c00_060e_2b3402060101);
    pub const CONSTANT_VALUE: Auid = id(0x0d010101_0101_3d00_060e_2b3402060101);
    pub const VARYING_VALUE: Auid = id(0x0d010101_0101_3e00_060e_2b3402060101);
    pub const DESCRIPTIVE_MARKER: Auid = id(0x0d010101_0101_4100_060e_2b3402060101);
    pub const SOUND_DESCRIPTOR: Auid = id(0x0d010101_0101_4200_060e_2b3402060101);
    pub const MULTIPLE_DESCRIPTOR: Auid = id(0x0d010101_0101_4400_060e_2b3402060101);
    pub const PCM_DESCRIPTOR: Auid = id(0x0d010101_0101_4800_060e_2b3402060101);
    pub const IMPORT_DESCRIPTOR: Auid = id(0x0d010101_0101_4a00_060e_2b3402060101);
}

/// The local property ids this crate uses.
pub mod pid {
    // Root
    pub const ROOT_META_DICTIONARY: u16 = 0x0001;
    pub const ROOT_HEADER: u16 = 0x0002;
    // MetaDefinition and its classes
    pub const META_IDENTIFICATION: u16 = 0x0005;
    pub const META_NAME: u16 = 0x0006;
    pub const META_DESCRIPTION: u16 = 0x0007;
    pub const CLASS_PARENT: u16 = 0x0008;
    pub const CLASS_PROPERTIES: u16 = 0x0009;
    pub const CLASS_IS_CONCRETE: u16 = 0x000a;
    pub const PROPERTY_TYPE: u16 = 0x000b;
    pub const PROPERTY_IS_OPTIONAL: u16 = 0x000c;
    pub const PROPERTY_LOCAL_ID: u16 = 0x000d;
    pub const PROPERTY_IS_UNIQUE: u16 = 0x000e;
    pub const INT_SIZE: u16 = 0x000f;
    pub const INT_SIGNED: u16 = 0x0010;
    pub const STRONG_REF_TYPE: u16 = 0x0011;
    pub const WEAK_REF_TYPE: u16 = 0x0012;
    pub const WEAK_TARGET_SET: u16 = 0x0013;
    pub const ENUM_TYPE: u16 = 0x0014;
    pub const ENUM_NAMES: u16 = 0x0015;
    pub const ENUM_VALUES: u16 = 0x0016;
    pub const FIXED_TYPE: u16 = 0x0017;
    pub const FIXED_COUNT: u16 = 0x0018;
    pub const VAR_TYPE: u16 = 0x0019;
    pub const SET_TYPE: u16 = 0x001a;
    pub const STRING_TYPE: u16 = 0x001b;
    pub const RECORD_TYPES: u16 = 0x001c;
    pub const RECORD_NAMES: u16 = 0x001d;
    pub const RENAME_TYPE: u16 = 0x001e;
    pub const EXT_ENUM_NAMES: u16 = 0x001f;
    pub const EXT_ENUM_VALUES: u16 = 0x0020;
    pub const META_CLASS_DEFINITIONS: u16 = 0x0003;
    pub const META_TYPE_DEFINITIONS: u16 = 0x0004;
    // InterchangeObject
    pub const OBJ_CLASS: u16 = 0x0101;
    // Component
    pub const COMPONENT_DATA_DEFINITION: u16 = 0x0201;
    pub const COMPONENT_LENGTH: u16 = 0x0202;
    // EssenceGroup
    pub const ESSENCE_GROUP_CHOICES: u16 = 0x0501;
    // Event
    pub const EVENT_POSITION: u16 = 0x0601;
    pub const EVENT_COMMENT: u16 = 0x0602;
    // OperationGroup
    pub const OPERATION_GROUP_OPERATION: u16 = 0x0b01;
    pub const OPERATION_GROUP_INPUT_SEGMENTS: u16 = 0x0b02;
    pub const OPERATION_GROUP_PARAMETERS: u16 = 0x0b03;
    // NestedScope
    pub const NESTED_SCOPE_SLOTS: u16 = 0x0c01;
    // Selector
    pub const SELECTOR_SELECTED: u16 = 0x0f01;
    // Sequence
    pub const SEQUENCE_COMPONENTS: u16 = 0x1001;
    // SourceReference / SourceClip
    pub const SOURCE_ID: u16 = 0x1101;
    pub const SOURCE_MOB_SLOT_ID: u16 = 0x1102;
    pub const SOURCE_CHANNEL_IDS: u16 = 0x1103;
    pub const SOURCE_CLIP_START_TIME: u16 = 0x1201;
    pub const SOURCE_CLIP_FADE_IN_LENGTH: u16 = 0x1202;
    pub const SOURCE_CLIP_FADE_IN_TYPE: u16 = 0x1203;
    pub const SOURCE_CLIP_FADE_OUT_LENGTH: u16 = 0x1204;
    pub const SOURCE_CLIP_FADE_OUT_TYPE: u16 = 0x1205;
    // Timecode
    pub const TIMECODE_START: u16 = 0x1501;
    pub const TIMECODE_FPS: u16 = 0x1502;
    pub const TIMECODE_DROP: u16 = 0x1503;
    // Transition
    pub const TRANSITION_OPERATION_GROUP: u16 = 0x1801;
    pub const TRANSITION_CUT_POINT: u16 = 0x1802;
    // ContentStorage
    pub const CONTENT_MOBS: u16 = 0x1901;
    pub const CONTENT_ESSENCE_DATA: u16 = 0x1902;
    // ControlPoint
    pub const CONTROL_POINT_VALUE: u16 = 0x1a02;
    pub const CONTROL_POINT_TIME: u16 = 0x1a03;
    pub const CONTROL_POINT_EDIT_HINT: u16 = 0x1a04;
    // DefinitionObject
    pub const DEFINITION_IDENTIFICATION: u16 = 0x1b01;
    pub const DEFINITION_NAME: u16 = 0x1b02;
    pub const DEFINITION_DESCRIPTION: u16 = 0x1b03;
    // OperationDefinition
    pub const OPERATION_DEF_DATA_DEFINITION: u16 = 0x1e01;
    pub const OPERATION_DEF_IS_TIME_WARP: u16 = 0x1e02;
    pub const OPERATION_DEF_CATEGORY: u16 = 0x1e06;
    pub const OPERATION_DEF_NUMBER_INPUTS: u16 = 0x1e07;
    pub const OPERATION_DEF_BYPASS: u16 = 0x1e08;
    pub const OPERATION_DEF_PARAMETERS_DEFINED: u16 = 0x1e09;
    // ParameterDefinition
    pub const PARAMETER_DEF_TYPE: u16 = 0x1f01;
    pub const PARAMETER_DEF_DISPLAY_UNITS: u16 = 0x1f03;
    // CodecDefinition
    pub const CODEC_DEF_FILE_DESCRIPTOR_CLASS: u16 = 0x2301;
    pub const CODEC_DEF_DATA_DEFINITIONS: u16 = 0x2302;
    // ContainerDefinition
    pub const CONTAINER_DEF_ESSENCE_IS_IDENTIFIED: u16 = 0x2401;
    // Dictionary
    pub const DICTIONARY_OPERATION_DEFINITIONS: u16 = 0x2603;
    pub const DICTIONARY_PARAMETER_DEFINITIONS: u16 = 0x2604;
    pub const DICTIONARY_DATA_DEFINITIONS: u16 = 0x2605;
    pub const DICTIONARY_CODEC_DEFINITIONS: u16 = 0x2607;
    pub const DICTIONARY_CONTAINER_DEFINITIONS: u16 = 0x2608;
    pub const DICTIONARY_INTERPOLATION_DEFINITIONS: u16 = 0x2609;
    // EssenceData
    pub const ESSENCE_DATA_MOB_ID: u16 = 0x2701;
    pub const ESSENCE_DATA_DATA: u16 = 0x2702;
    // EssenceDescriptor / FileDescriptor
    pub const ESSENCE_DESCRIPTOR_LOCATOR: u16 = 0x2f01;
    pub const FILE_DESCRIPTOR_SAMPLE_RATE: u16 = 0x3001;
    pub const FILE_DESCRIPTOR_LENGTH: u16 = 0x3002;
    pub const FILE_DESCRIPTOR_CONTAINER_FORMAT: u16 = 0x3004;
    pub const FILE_DESCRIPTOR_CODEC: u16 = 0x3005;
    pub const FILE_DESCRIPTOR_LINKED_SLOT_ID: u16 = 0x3006;
    pub const MULTIPLE_FILE_DESCRIPTORS: u16 = 0x3f01;
    pub const AIFC_SUMMARY: u16 = 0x3101;
    pub const WAVE_SUMMARY: u16 = 0x3801;
    // Header
    pub const HEADER_BYTE_ORDER: u16 = 0x3b01;
    pub const HEADER_LAST_MODIFIED: u16 = 0x3b02;
    pub const HEADER_CONTENT: u16 = 0x3b03;
    pub const HEADER_DICTIONARY: u16 = 0x3b04;
    pub const HEADER_VERSION: u16 = 0x3b05;
    pub const HEADER_IDENTIFICATION_LIST: u16 = 0x3b06;
    pub const HEADER_OBJECT_MODEL_VERSION: u16 = 0x3b07;
    pub const HEADER_OPERATIONAL_PATTERN: u16 = 0x3b09;
    pub const HEADER_ESSENCE_CONTAINERS: u16 = 0x3b0a;
    // Identification
    pub const IDENT_COMPANY_NAME: u16 = 0x3c01;
    pub const IDENT_PRODUCT_NAME: u16 = 0x3c02;
    pub const IDENT_PRODUCT_VERSION: u16 = 0x3c03;
    pub const IDENT_PRODUCT_VERSION_STRING: u16 = 0x3c04;
    pub const IDENT_PRODUCT_ID: u16 = 0x3c05;
    pub const IDENT_DATE: u16 = 0x3c06;
    pub const IDENT_PLATFORM: u16 = 0x3c08;
    pub const IDENT_GENERATION_AUID: u16 = 0x3c09;
    // SoundDescriptor / PCMDescriptor
    pub const SOUND_QUANTIZATION_BITS: u16 = 0x3d01;
    pub const SOUND_AUDIO_SAMPLING_RATE: u16 = 0x3d03;
    pub const SOUND_CHANNELS: u16 = 0x3d07;
    pub const PCM_AVERAGE_BPS: u16 = 0x3d09;
    pub const PCM_BLOCK_ALIGN: u16 = 0x3d0a;
    // NetworkLocator
    pub const NETWORK_LOCATOR_URL: u16 = 0x4001;
    // Mob
    pub const MOB_MOB_ID: u16 = 0x4401;
    pub const MOB_NAME: u16 = 0x4402;
    pub const MOB_SLOTS: u16 = 0x4403;
    pub const MOB_LAST_MODIFIED: u16 = 0x4404;
    pub const MOB_CREATION_TIME: u16 = 0x4405;
    pub const MOB_USAGE_CODE: u16 = 0x4408;
    // SourceMob
    pub const SOURCE_MOB_ESSENCE_DESCRIPTION: u16 = 0x4701;
    // MobSlot
    pub const SLOT_ID: u16 = 0x4801;
    pub const SLOT_NAME: u16 = 0x4802;
    pub const SLOT_SEGMENT: u16 = 0x4803;
    pub const SLOT_PHYSICAL_TRACK_NUMBER: u16 = 0x4804;
    // EventMobSlot / TimelineMobSlot
    pub const EVENT_SLOT_EDIT_RATE: u16 = 0x4901;
    pub const TIMELINE_SLOT_EDIT_RATE: u16 = 0x4b01;
    pub const TIMELINE_SLOT_ORIGIN: u16 = 0x4b02;
    // Parameter, ConstantValue, VaryingValue
    pub const PARAMETER_DEFINITION: u16 = 0x4c01;
    pub const CONSTANT_VALUE_VALUE: u16 = 0x4d01;
    pub const VARYING_VALUE_INTERPOLATION: u16 = 0x4e01;
    pub const VARYING_VALUE_POINT_LIST: u16 = 0x4e02;
    // DescriptiveMarker
    pub const DESCRIPTIVE_MARKER_DESCRIBED_SLOTS: u16 = 0x6102;
}

/// The types this crate writes values of.
pub mod ty {
    use crate::ids::{Auid, id};

    pub const RATIONAL: Auid = id(0x03010100_0000_0000_060e_2b3401040101);
    pub const DATA_DEFINITION_WEAK: Auid = id(0x05010300_0000_0000_060e_2b3401040101);
    pub const CONTAINER_DEFINITION_WEAK: Auid = id(0x05010200_0000_0000_060e_2b3401040101);
    pub const OPERATION_DEFINITION_WEAK: Auid = id(0x05010700_0000_0000_060e_2b3401040101);
    pub const INTERPOLATION_DEFINITION_WEAK: Auid = id(0x05010500_0000_0000_060e_2b3401040101);
    pub const PARAMETER_DEFINITION_WEAK_SET: Auid = id(0x05030e00_0000_0000_060e_2b3401040101);
    pub const TYPE_DEFINITION_WEAK: Auid = id(0x05010900_0000_0000_060e_2b3401040101);
    pub const CLASS_DEFINITION_WEAK: Auid = id(0x05010100_0000_0000_060e_2b3401040101);
    pub const CODEC_DEFINITION_WEAK: Auid = id(0x05010b00_0000_0000_060e_2b3401040101);
    pub const DATA_DEFINITION_WEAK_VECTOR: Auid = id(0x05040300_0000_0000_060e_2b3401040101);
}

#[cfg(test)]
mod tests;
