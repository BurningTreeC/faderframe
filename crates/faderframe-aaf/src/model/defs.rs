//! Definitions a file's dictionary holds (AAF Object Specification,
//! AAF Edit Protocol): data kinds, containers, codecs, operations, their
//! parameters, interpolations and the labels mobs carry.

use crate::ids::{Auid, id};

// Data definitions.
pub const DATA_PICTURE: Auid = id(0x01030202_0100_0000_060e_2b3404010101);
pub const DATA_SOUND: Auid = id(0x01030202_0200_0000_060e_2b3404010101);
pub const DATA_TIMECODE: Auid = id(0x01030201_0100_0000_060e_2b3404010101);
pub const DATA_DESCRIPTIVE_METADATA: Auid = id(0x01030201_1000_0000_060e_2b3404010101);
pub const DATA_LEGACY_PICTURE: Auid = id(0x6f3c8ce1_6cef_11d2_807d_006008143e6f);
pub const DATA_LEGACY_SOUND: Auid = id(0x78e1ebe1_6cef_11d2_807d_006008143e6f);
pub const DATA_LEGACY_TIMECODE: Auid = id(0x7f275e81_77e5_11d2_807f_006008143e6f);
pub const DATA_EDGECODE: Auid = id(0xd2bb2af0_d234_11d2_89ee_006097116212);

/// The data definitions a writer puts in every file, with their names.
pub const DATA_DEFINITIONS: &[(Auid, &str, &str)] = &[
    (DATA_PICTURE, "Picture", "Picture data"),
    (DATA_SOUND, "Sound", "Sound data"),
    (DATA_TIMECODE, "Timecode", "Timecode data"),
    (
        DATA_DESCRIPTIVE_METADATA,
        "DescriptiveMetadata",
        "Descriptive metadata",
    ),
    (DATA_LEGACY_SOUND, "LegacySound", "Sound data (legacy)"),
    (
        DATA_LEGACY_TIMECODE,
        "LegacyTimecode",
        "Timecode data (legacy)",
    ),
];

/// Sound, by either label.
pub fn is_sound(d: Auid) -> bool {
    d == DATA_SOUND || d == DATA_LEGACY_SOUND
}

/// Timecode, by either label.
pub fn is_timecode(d: Auid) -> bool {
    d == DATA_TIMECODE || d == DATA_LEGACY_TIMECODE
}

// Containers: where essence is.
pub const CONTAINER_EXTERNAL: Auid = id(0x4313b572_d8ba_11d2_809b_006008143e6f);
pub const CONTAINER_AAF: Auid = id(0x4313b571_d8ba_11d2_809b_006008143e6f);
pub const CONTAINER_RIFF_WAVE: Auid = id(0x0d011301_0101_0100_060e_2b3404010106);
pub const CONTAINER_AIFF_AIFC: Auid = id(0x0d011301_0104_0100_060e_2b3404010106);

pub const CONTAINER_DEFINITIONS: &[(Auid, &str)] = &[
    (CONTAINER_EXTERNAL, "External"),
    (CONTAINER_AAF, "AAF"),
    (CONTAINER_RIFF_WAVE, "RIFFWAVE"),
];

// Codecs.
pub const CODEC_PCM: Auid = id(0x90ac17c8_e3e2_4596_9e9e_a6dd1c70c892);
pub const CODEC_WAVE: Auid = id(0x820f09b1_eb9b_11d2_809f_006008143e6f);
pub const CODEC_AIFC: Auid = id(0x4b1c1a45_03f2_11d4_80fb_006008143e6f);

// Operations and their parameters.
pub const OPERATION_MONO_AUDIO_GAIN: Auid = id(0x9d2ea894_0968_11d3_8a38_0050040ef7d2);
pub const OPERATION_STEREO_AUDIO_GAIN: Auid = id(0x9d2ea895_0968_11d3_8a38_0050040ef7d2);
pub const OPERATION_MONO_AUDIO_PAN: Auid = id(0x9d2ea893_0968_11d3_8a38_0050040ef7d2);
pub const OPERATION_MONO_AUDIO_DISSOLVE: Auid = id(0x0c3bea41_fc05_11d2_8a29_0050040ef7d2);
pub const OPERATION_TWO_PARAMETER_MONO_AUDIO_DISSOLVE: Auid =
    id(0x2311bd90_b5da_4285_aa3a_8552848779b3);
pub const OPERATION_STEREO_AUDIO_DISSOLVE: Auid = id(0x0c3bea42_fc05_11d2_8a29_0050040ef7d2);

pub const PARAMETER_LEVEL: Auid = id(0xe4962320_2267_11d3_8a4c_0050040ef7d2);
pub const PARAMETER_AMPLITUDE: Auid = id(0xe4962321_2267_11d3_8a4c_0050040ef7d2);
pub const PARAMETER_PAN: Auid = id(0xe4962322_2267_11d3_8a4c_0050040ef7d2);
pub const PARAMETER_OUTGOING_LEVEL: Auid = id(0x9e610007_1be2_41e1_bb11_c95de9964d03);
pub const PARAMETER_INCOMING_LEVEL: Auid = id(0x48cea642_a8f9_455b_82b3_86c814b797c7);

pub const OPERATION_CATEGORY_EFFECT: Auid = id(0x0d010102_0101_0100_060e_2b3404010101);

// Interpolations.
pub const INTERPOLATION_NONE: Auid = id(0x5b6c85a3_0ede_11d3_80a9_006008143e6f);
pub const INTERPOLATION_LINEAR: Auid = id(0x5b6c85a4_0ede_11d3_80a9_006008143e6f);
pub const INTERPOLATION_CONSTANT: Auid = id(0x5b6c85a5_0ede_11d3_80a9_006008143e6f);
pub const INTERPOLATION_BSPLINE: Auid = id(0x5b6c85a6_0ede_11d3_80a9_006008143e6f);
pub const INTERPOLATION_LOG: Auid = id(0x15829ec3_1f24_458a_960d_c65bb23c2aa1);
pub const INTERPOLATION_POWER: Auid = id(0xc09153f7_bd18_4e5a_ad09_cbdd654fa001);

pub const INTERPOLATION_DEFINITIONS: &[(Auid, &str)] = &[
    (INTERPOLATION_LINEAR, "LinearInterp"),
    (INTERPOLATION_CONSTANT, "ConstantInterp"),
];

// Mob usage and the operational pattern.
pub const USAGE_TOP_LEVEL: Auid = id(0x0d010102_0101_0700_060e_2b3404010101);
pub const USAGE_LOWER_LEVEL: Auid = id(0x0d010102_0101_0800_060e_2b3404010101);
pub const USAGE_SUB_CLIP: Auid = id(0x0d010102_0101_0500_060e_2b3404010101);
pub const OP_EDIT_PROTOCOL: Auid = id(0x0d011201_0100_0000_060e_2b3404010105);

/// FaderFrame as a product (Identification::ProductID).
pub const PRODUCT_ID: Auid = id(0x4f7a1c2e_5b8d_4e61_9a03_f4dc8e21b6a7);
