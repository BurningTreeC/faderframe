use super::*;

#[test]
fn the_named_classes_are_the_tables() {
    let named = [
        (cls::ROOT, "Root"),
        (cls::META_DICTIONARY, "MetaDictionary"),
        (cls::CLASS_DEFINITION, "ClassDefinition"),
        (cls::PROPERTY_DEFINITION, "PropertyDefinition"),
        (cls::TYPE_DEF_INT, "TypeDefinitionInteger"),
        (
            cls::TYPE_DEF_STRONG_REF,
            "TypeDefinitionStrongObjectReference",
        ),
        (cls::TYPE_DEF_WEAK_REF, "TypeDefinitionWeakObjectReference"),
        (cls::TYPE_DEF_ENUM, "TypeDefinitionEnumeration"),
        (cls::TYPE_DEF_FIXED_ARRAY, "TypeDefinitionFixedArray"),
        (cls::TYPE_DEF_VAR_ARRAY, "TypeDefinitionVariableArray"),
        (cls::TYPE_DEF_SET, "TypeDefinitionSet"),
        (cls::TYPE_DEF_STRING, "TypeDefinitionString"),
        (cls::TYPE_DEF_STREAM, "TypeDefinitionStream"),
        (cls::TYPE_DEF_RECORD, "TypeDefinitionRecord"),
        (cls::TYPE_DEF_RENAME, "TypeDefinitionRename"),
        (
            cls::TYPE_DEF_EXT_ENUM,
            "TypeDefinitionExtendibleEnumeration",
        ),
        (cls::TYPE_DEF_INDIRECT, "TypeDefinitionIndirect"),
        (cls::TYPE_DEF_OPAQUE, "TypeDefinitionOpaque"),
        (cls::TYPE_DEF_CHARACTER, "TypeDefinitionCharacter"),
        (
            cls::TYPE_DEF_GENERIC_CHARACTER,
            "TypeDefinitionGenericCharacter",
        ),
        (cls::COMPONENT, "Component"),
        (cls::SEGMENT, "Segment"),
        (cls::ESSENCE_GROUP, "EssenceGroup"),
        (cls::EVENT, "Event"),
        (cls::COMMENT_MARKER, "CommentMarker"),
        (cls::FILLER, "Filler"),
        (cls::OPERATION_GROUP, "OperationGroup"),
        (cls::NESTED_SCOPE, "NestedScope"),
        (cls::SCOPE_REFERENCE, "ScopeReference"),
        (cls::SELECTOR, "Selector"),
        (cls::SEQUENCE, "Sequence"),
        (cls::SOURCE_CLIP, "SourceClip"),
        (cls::TIMECODE, "Timecode"),
        (cls::TRANSITION, "Transition"),
        (cls::CONTENT_STORAGE, "ContentStorage"),
        (cls::CONTROL_POINT, "ControlPoint"),
        (cls::DATA_DEFINITION, "DataDefinition"),
        (cls::OPERATION_DEFINITION, "OperationDefinition"),
        (cls::PARAMETER_DEFINITION, "ParameterDefinition"),
        (cls::CODEC_DEFINITION, "CodecDefinition"),
        (cls::CONTAINER_DEFINITION, "ContainerDefinition"),
        (cls::INTERPOLATION_DEFINITION, "InterpolationDefinition"),
        (cls::DICTIONARY, "Dictionary"),
        (cls::ESSENCE_DATA, "EssenceData"),
        (cls::ESSENCE_DESCRIPTOR, "EssenceDescriptor"),
        (cls::FILE_DESCRIPTOR, "FileDescriptor"),
        (cls::AIFC_DESCRIPTOR, "AIFCDescriptor"),
        (cls::WAVE_DESCRIPTOR, "WAVEDescriptor"),
        (cls::TAPE_DESCRIPTOR, "TapeDescriptor"),
        (cls::HEADER, "Header"),
        (cls::IDENTIFICATION, "Identification"),
        (cls::NETWORK_LOCATOR, "NetworkLocator"),
        (cls::MOB, "Mob"),
        (cls::COMPOSITION_MOB, "CompositionMob"),
        (cls::MASTER_MOB, "MasterMob"),
        (cls::SOURCE_MOB, "SourceMob"),
        (cls::MOB_SLOT, "MobSlot"),
        (cls::EVENT_MOB_SLOT, "EventMobSlot"),
        (cls::TIMELINE_MOB_SLOT, "TimelineMobSlot"),
        (cls::PARAMETER, "Parameter"),
        (cls::CONSTANT_VALUE, "ConstantValue"),
        (cls::VARYING_VALUE, "VaryingValue"),
        (cls::DESCRIPTIVE_MARKER, "DescriptiveMarker"),
        (cls::SOUND_DESCRIPTOR, "SoundDescriptor"),
        (cls::MULTIPLE_DESCRIPTOR, "MultipleDescriptor"),
        (cls::PCM_DESCRIPTOR, "PCMDescriptor"),
        (cls::IMPORT_DESCRIPTOR, "ImportDescriptor"),
    ];
    for (id, name) in named {
        assert_eq!(class(id).map(|c| c.name), Some(name), "{name}");
    }
}

#[test]
fn the_named_properties_are_the_tables() {
    let named = [
        (pid::ROOT_META_DICTIONARY, "Root", "MetaDictionary"),
        (pid::ROOT_HEADER, "Root", "Header"),
        (
            pid::META_IDENTIFICATION,
            "ClassDefinition",
            "Identification",
        ),
        (pid::META_NAME, "ClassDefinition", "Name"),
        (pid::META_DESCRIPTION, "ClassDefinition", "Description"),
        (pid::CLASS_PARENT, "ClassDefinition", "ParentClass"),
        (pid::CLASS_PROPERTIES, "ClassDefinition", "Properties"),
        (pid::CLASS_IS_CONCRETE, "ClassDefinition", "IsConcrete"),
        (pid::PROPERTY_TYPE, "PropertyDefinition", "Type"),
        (
            pid::PROPERTY_IS_OPTIONAL,
            "PropertyDefinition",
            "IsOptional",
        ),
        (
            pid::PROPERTY_LOCAL_ID,
            "PropertyDefinition",
            "LocalIdentification",
        ),
        (
            pid::PROPERTY_IS_UNIQUE,
            "PropertyDefinition",
            "IsUniqueIdentifier",
        ),
        (pid::INT_SIZE, "TypeDefinitionInteger", "Size"),
        (pid::INT_SIGNED, "TypeDefinitionInteger", "IsSigned"),
        (
            pid::STRONG_REF_TYPE,
            "TypeDefinitionStrongObjectReference",
            "ReferencedType",
        ),
        (
            pid::WEAK_REF_TYPE,
            "TypeDefinitionWeakObjectReference",
            "ReferencedType",
        ),
        (
            pid::WEAK_TARGET_SET,
            "TypeDefinitionWeakObjectReference",
            "TargetSet",
        ),
        (pid::ENUM_TYPE, "TypeDefinitionEnumeration", "ElementType"),
        (pid::ENUM_NAMES, "TypeDefinitionEnumeration", "ElementNames"),
        (
            pid::ENUM_VALUES,
            "TypeDefinitionEnumeration",
            "ElementValues",
        ),
        (pid::FIXED_TYPE, "TypeDefinitionFixedArray", "ElementType"),
        (pid::FIXED_COUNT, "TypeDefinitionFixedArray", "ElementCount"),
        (pid::VAR_TYPE, "TypeDefinitionVariableArray", "ElementType"),
        (pid::SET_TYPE, "TypeDefinitionSet", "ElementType"),
        (pid::STRING_TYPE, "TypeDefinitionString", "ElementType"),
        (pid::RECORD_TYPES, "TypeDefinitionRecord", "MemberTypes"),
        (pid::RECORD_NAMES, "TypeDefinitionRecord", "MemberNames"),
        (pid::RENAME_TYPE, "TypeDefinitionRename", "RenamedType"),
        (
            pid::EXT_ENUM_NAMES,
            "TypeDefinitionExtendibleEnumeration",
            "ElementNames",
        ),
        (
            pid::EXT_ENUM_VALUES,
            "TypeDefinitionExtendibleEnumeration",
            "ElementValues",
        ),
        (
            pid::META_CLASS_DEFINITIONS,
            "MetaDictionary",
            "ClassDefinitions",
        ),
        (
            pid::META_TYPE_DEFINITIONS,
            "MetaDictionary",
            "TypeDefinitions",
        ),
        (pid::OBJ_CLASS, "InterchangeObject", "ObjClass"),
        (
            pid::COMPONENT_DATA_DEFINITION,
            "Component",
            "DataDefinition",
        ),
        (pid::COMPONENT_LENGTH, "Component", "Length"),
        (pid::ESSENCE_GROUP_CHOICES, "EssenceGroup", "Choices"),
        (pid::EVENT_POSITION, "Event", "Position"),
        (pid::EVENT_COMMENT, "Event", "Comment"),
        (
            pid::OPERATION_GROUP_OPERATION,
            "OperationGroup",
            "Operation",
        ),
        (
            pid::OPERATION_GROUP_INPUT_SEGMENTS,
            "OperationGroup",
            "InputSegments",
        ),
        (
            pid::OPERATION_GROUP_PARAMETERS,
            "OperationGroup",
            "Parameters",
        ),
        (pid::NESTED_SCOPE_SLOTS, "NestedScope", "Slots"),
        (pid::SELECTOR_SELECTED, "Selector", "Selected"),
        (pid::SEQUENCE_COMPONENTS, "Sequence", "Components"),
        (pid::SOURCE_ID, "SourceReference", "SourceID"),
        (
            pid::SOURCE_MOB_SLOT_ID,
            "SourceReference",
            "SourceMobSlotID",
        ),
        (pid::SOURCE_CHANNEL_IDS, "SourceReference", "ChannelIDs"),
        (pid::SOURCE_CLIP_START_TIME, "SourceClip", "StartTime"),
        (
            pid::SOURCE_CLIP_FADE_IN_LENGTH,
            "SourceClip",
            "FadeInLength",
        ),
        (pid::SOURCE_CLIP_FADE_IN_TYPE, "SourceClip", "FadeInType"),
        (
            pid::SOURCE_CLIP_FADE_OUT_LENGTH,
            "SourceClip",
            "FadeOutLength",
        ),
        (pid::SOURCE_CLIP_FADE_OUT_TYPE, "SourceClip", "FadeOutType"),
        (pid::TIMECODE_START, "Timecode", "Start"),
        (pid::TIMECODE_FPS, "Timecode", "FPS"),
        (pid::TIMECODE_DROP, "Timecode", "Drop"),
        (
            pid::TRANSITION_OPERATION_GROUP,
            "Transition",
            "OperationGroup",
        ),
        (pid::TRANSITION_CUT_POINT, "Transition", "CutPoint"),
        (pid::CONTENT_MOBS, "ContentStorage", "Mobs"),
        (pid::CONTENT_ESSENCE_DATA, "ContentStorage", "EssenceData"),
        (pid::CONTROL_POINT_VALUE, "ControlPoint", "Value"),
        (pid::CONTROL_POINT_TIME, "ControlPoint", "Time"),
        (pid::CONTROL_POINT_EDIT_HINT, "ControlPoint", "EditHint"),
        (
            pid::DEFINITION_IDENTIFICATION,
            "DefinitionObject",
            "Identification",
        ),
        (pid::DEFINITION_NAME, "DefinitionObject", "Name"),
        (
            pid::DEFINITION_DESCRIPTION,
            "DefinitionObject",
            "Description",
        ),
        (
            pid::OPERATION_DEF_DATA_DEFINITION,
            "OperationDefinition",
            "DataDefinition",
        ),
        (
            pid::OPERATION_DEF_IS_TIME_WARP,
            "OperationDefinition",
            "IsTimeWarp",
        ),
        (
            pid::OPERATION_DEF_CATEGORY,
            "OperationDefinition",
            "OperationCategory",
        ),
        (
            pid::OPERATION_DEF_NUMBER_INPUTS,
            "OperationDefinition",
            "NumberInputs",
        ),
        (pid::OPERATION_DEF_BYPASS, "OperationDefinition", "Bypass"),
        (
            pid::OPERATION_DEF_PARAMETERS_DEFINED,
            "OperationDefinition",
            "ParametersDefined",
        ),
        (pid::PARAMETER_DEF_TYPE, "ParameterDefinition", "Type"),
        (
            pid::PARAMETER_DEF_DISPLAY_UNITS,
            "ParameterDefinition",
            "DisplayUnits",
        ),
        (
            pid::CODEC_DEF_FILE_DESCRIPTOR_CLASS,
            "CodecDefinition",
            "FileDescriptorClass",
        ),
        (
            pid::CODEC_DEF_DATA_DEFINITIONS,
            "CodecDefinition",
            "DataDefinitions",
        ),
        (
            pid::CONTAINER_DEF_ESSENCE_IS_IDENTIFIED,
            "ContainerDefinition",
            "EssenceIsIdentified",
        ),
        (
            pid::DICTIONARY_OPERATION_DEFINITIONS,
            "Dictionary",
            "OperationDefinitions",
        ),
        (
            pid::DICTIONARY_PARAMETER_DEFINITIONS,
            "Dictionary",
            "ParameterDefinitions",
        ),
        (
            pid::DICTIONARY_DATA_DEFINITIONS,
            "Dictionary",
            "DataDefinitions",
        ),
        (
            pid::DICTIONARY_CODEC_DEFINITIONS,
            "Dictionary",
            "CodecDefinitions",
        ),
        (
            pid::DICTIONARY_CONTAINER_DEFINITIONS,
            "Dictionary",
            "ContainerDefinitions",
        ),
        (
            pid::DICTIONARY_INTERPOLATION_DEFINITIONS,
            "Dictionary",
            "InterpolationDefinitions",
        ),
        (pid::ESSENCE_DATA_MOB_ID, "EssenceData", "MobID"),
        (pid::ESSENCE_DATA_DATA, "EssenceData", "Data"),
        (
            pid::ESSENCE_DESCRIPTOR_LOCATOR,
            "EssenceDescriptor",
            "Locator",
        ),
        (
            pid::FILE_DESCRIPTOR_SAMPLE_RATE,
            "FileDescriptor",
            "SampleRate",
        ),
        (pid::FILE_DESCRIPTOR_LENGTH, "FileDescriptor", "Length"),
        (
            pid::FILE_DESCRIPTOR_CONTAINER_FORMAT,
            "FileDescriptor",
            "ContainerFormat",
        ),
        (
            pid::FILE_DESCRIPTOR_CODEC,
            "FileDescriptor",
            "CodecDefinition",
        ),
        (
            pid::FILE_DESCRIPTOR_LINKED_SLOT_ID,
            "FileDescriptor",
            "LinkedSlotID",
        ),
        (
            pid::MULTIPLE_FILE_DESCRIPTORS,
            "MultipleDescriptor",
            "FileDescriptors",
        ),
        (pid::AIFC_SUMMARY, "AIFCDescriptor", "Summary"),
        (pid::WAVE_SUMMARY, "WAVEDescriptor", "Summary"),
        (pid::HEADER_BYTE_ORDER, "Header", "ByteOrder"),
        (pid::HEADER_LAST_MODIFIED, "Header", "LastModified"),
        (pid::HEADER_CONTENT, "Header", "Content"),
        (pid::HEADER_DICTIONARY, "Header", "Dictionary"),
        (pid::HEADER_VERSION, "Header", "Version"),
        (
            pid::HEADER_IDENTIFICATION_LIST,
            "Header",
            "IdentificationList",
        ),
        (
            pid::HEADER_OBJECT_MODEL_VERSION,
            "Header",
            "ObjectModelVersion",
        ),
        (
            pid::HEADER_OPERATIONAL_PATTERN,
            "Header",
            "OperationalPattern",
        ),
        (
            pid::HEADER_ESSENCE_CONTAINERS,
            "Header",
            "EssenceContainers",
        ),
        (pid::IDENT_COMPANY_NAME, "Identification", "CompanyName"),
        (pid::IDENT_PRODUCT_NAME, "Identification", "ProductName"),
        (
            pid::IDENT_PRODUCT_VERSION,
            "Identification",
            "ProductVersion",
        ),
        (
            pid::IDENT_PRODUCT_VERSION_STRING,
            "Identification",
            "ProductVersionString",
        ),
        (pid::IDENT_PRODUCT_ID, "Identification", "ProductID"),
        (pid::IDENT_DATE, "Identification", "Date"),
        (pid::IDENT_PLATFORM, "Identification", "Platform"),
        (
            pid::IDENT_GENERATION_AUID,
            "Identification",
            "GenerationAUID",
        ),
        (
            pid::SOUND_QUANTIZATION_BITS,
            "SoundDescriptor",
            "QuantizationBits",
        ),
        (
            pid::SOUND_AUDIO_SAMPLING_RATE,
            "SoundDescriptor",
            "AudioSamplingRate",
        ),
        (pid::SOUND_CHANNELS, "SoundDescriptor", "Channels"),
        (pid::PCM_AVERAGE_BPS, "PCMDescriptor", "AverageBPS"),
        (pid::PCM_BLOCK_ALIGN, "PCMDescriptor", "BlockAlign"),
        (pid::NETWORK_LOCATOR_URL, "NetworkLocator", "URLString"),
        (pid::MOB_MOB_ID, "Mob", "MobID"),
        (pid::MOB_NAME, "Mob", "Name"),
        (pid::MOB_SLOTS, "Mob", "Slots"),
        (pid::MOB_LAST_MODIFIED, "Mob", "LastModified"),
        (pid::MOB_CREATION_TIME, "Mob", "CreationTime"),
        (pid::MOB_USAGE_CODE, "Mob", "UsageCode"),
        (
            pid::SOURCE_MOB_ESSENCE_DESCRIPTION,
            "SourceMob",
            "EssenceDescription",
        ),
        (pid::SLOT_ID, "MobSlot", "SlotID"),
        (pid::SLOT_NAME, "MobSlot", "SlotName"),
        (pid::SLOT_SEGMENT, "MobSlot", "Segment"),
        (
            pid::SLOT_PHYSICAL_TRACK_NUMBER,
            "MobSlot",
            "PhysicalTrackNumber",
        ),
        (pid::EVENT_SLOT_EDIT_RATE, "EventMobSlot", "EditRate"),
        (pid::TIMELINE_SLOT_EDIT_RATE, "TimelineMobSlot", "EditRate"),
        (pid::TIMELINE_SLOT_ORIGIN, "TimelineMobSlot", "Origin"),
        (pid::PARAMETER_DEFINITION, "Parameter", "Definition"),
        (pid::CONSTANT_VALUE_VALUE, "ConstantValue", "Value"),
        (
            pid::VARYING_VALUE_INTERPOLATION,
            "VaryingValue",
            "Interpolation",
        ),
        (pid::VARYING_VALUE_POINT_LIST, "VaryingValue", "PointList"),
        (
            pid::DESCRIPTIVE_MARKER_DESCRIBED_SLOTS,
            "DescriptiveMarker",
            "DescribedSlots",
        ),
    ];
    for (pid, class, prop) in named {
        let c = class_named(class).unwrap_or_else(|| panic!("{class}"));
        let p = property(c.id, prop).unwrap_or_else(|| panic!("{class}.{prop}"));
        assert_eq!(p.pid, pid, "{class}.{prop}");
    }
}

#[test]
fn the_named_types_are_the_tables() {
    let named = [
        (ty::RATIONAL, "Rational"),
        (ty::DATA_DEFINITION_WEAK, "DataDefinitionWeakReference"),
        (
            ty::CONTAINER_DEFINITION_WEAK,
            "ContainerDefinitionWeakReference",
        ),
        (
            ty::OPERATION_DEFINITION_WEAK,
            "OperationDefinitionWeakReference",
        ),
        (
            ty::INTERPOLATION_DEFINITION_WEAK,
            "InterpolationDefinitionWeakReference",
        ),
        (
            ty::PARAMETER_DEFINITION_WEAK_SET,
            "kAAFTypeID_ParameterDefinitionWeakReferenceSet",
        ),
        (ty::TYPE_DEFINITION_WEAK, "TypeDefinitionWeakReference"),
        (ty::CLASS_DEFINITION_WEAK, "ClassDefinitionWeakReference"),
        (ty::CODEC_DEFINITION_WEAK, "CodecDefinitionWeakReference"),
        (
            ty::DATA_DEFINITION_WEAK_VECTOR,
            "kAAFTypeID_DataDefinitionWeakReferenceVector",
        ),
    ];
    for (id, name) in named {
        assert_eq!(type_def(id).map(|t| t.name), Some(name));
    }
}

#[test]
fn weak_references_point_into_the_dictionaries() {
    assert_eq!(
        weak_path(ty::CLASS_DEFINITION_WEAK),
        Some(vec![0x0001, 0x0003])
    );
    assert_eq!(
        weak_path(ty::TYPE_DEFINITION_WEAK),
        Some(vec![0x0001, 0x0004])
    );
    assert_eq!(
        weak_path(ty::DATA_DEFINITION_WEAK),
        Some(vec![0x0002, 0x3b04, 0x2605])
    );
    assert_eq!(
        weak_path(ty::PARAMETER_DEFINITION_WEAK_SET),
        Some(vec![0x0002, 0x3b04, 0x2604])
    );
    assert!(is_a(cls::PCM_DESCRIPTOR, cls::FILE_DESCRIPTOR));
    assert!(is_a(cls::DESCRIPTIVE_MARKER, cls::EVENT));
    assert!(!is_a(cls::SOURCE_CLIP, cls::OPERATION_GROUP));
}
