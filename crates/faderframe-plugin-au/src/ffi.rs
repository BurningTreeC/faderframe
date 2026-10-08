//! The parts of AudioToolbox and CoreFoundation the host uses, declared by
//! hand from Apple's C headers (`AudioComponent.h`, `AUComponent.h`,
//! `AudioUnitProperties.h`, `MusicDevice.h`, `AudioUnitUtilities.h`). The
//! AUv2 C API has been stable for two decades; AUv3 units are reached
//! through the same API (the system bridges them).

#![allow(
    non_snake_case,
    non_camel_case_types,
    non_upper_case_globals,
    dead_code
)]

use std::ffi::{c_char, c_void};

pub type OSStatus = i32;
pub type OSType = u32;
pub type Boolean = u8;
pub type AudioComponent = *mut c_void;
pub type AudioUnit = *mut c_void;
pub type CFTypeRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFDataRef = *const c_void;
pub type CFURLRef = *const c_void;
pub type CFPropertyListRef = *const c_void;
pub type CFAllocatorRef = *const c_void;
pub type CFRunLoopRef = *mut c_void;
pub type CFIndex = isize;
pub type AUEventListenerRef = *mut c_void;

pub const fn fourcc(s: &[u8; 4]) -> OSType {
    u32::from_be_bytes(*s)
}

// Component types.
pub const kAudioUnitType_Effect: OSType = fourcc(b"aufx");
pub const kAudioUnitType_MusicEffect: OSType = fourcc(b"aumf");
pub const kAudioUnitType_MusicDevice: OSType = fourcc(b"aumu");

// Scopes.
pub const kAudioUnitScope_Global: u32 = 0;
pub const kAudioUnitScope_Input: u32 = 1;
pub const kAudioUnitScope_Output: u32 = 2;

// Properties.
pub const kAudioUnitProperty_ClassInfo: u32 = 0;
pub const kAudioUnitProperty_ParameterList: u32 = 3;
pub const kAudioUnitProperty_ParameterInfo: u32 = 4;
pub const kAudioUnitProperty_StreamFormat: u32 = 8;
pub const kAudioUnitProperty_ElementCount: u32 = 11;
pub const kAudioUnitProperty_Latency: u32 = 12;
pub const kAudioUnitProperty_MaximumFramesPerSlice: u32 = 14;
pub const kAudioUnitProperty_TailTime: u32 = 20;
pub const kAudioUnitProperty_SetRenderCallback: u32 = 23;
pub const kAudioUnitProperty_HostCallbacks: u32 = 27;
pub const kAudioUnitProperty_ElementName: u32 = 30;
pub const kAudioUnitProperty_CocoaUI: u32 = 31;
pub const kAudioUnitProperty_ParameterStringFromValue: u32 = 33;

// Parameter flags and units.
pub const kAudioUnitParameterFlag_CFNameRelease: u32 = 1 << 4;
pub const kAudioUnitParameterFlag_MeterReadOnly: u32 = 1 << 15;
pub const kAudioUnitParameterFlag_ValuesHaveStrings: u32 = 1 << 21;
pub const kAudioUnitParameterFlag_NonRealTime: u32 = 1 << 24;
pub const kAudioUnitParameterFlag_HasCFNameString: u32 = 1 << 27;
pub const kAudioUnitParameterFlag_IsReadable: u32 = 1 << 30;
pub const kAudioUnitParameterFlag_IsWritable: u32 = 1 << 31;
pub const kAudioUnitParameterUnit_Indexed: u32 = 1;
pub const kAudioUnitParameterUnit_Boolean: u32 = 2;
pub const kAudioUnitParameterUnit_Seconds: u32 = 4;
pub const kAudioUnitParameterUnit_SampleFrames: u32 = 5;
pub const kAudioUnitParameterUnit_Hertz: u32 = 8;
pub const kAudioUnitParameterUnit_Decibels: u32 = 13;
pub const kAudioUnitParameterUnit_LinearGain: u32 = 14;
pub const kAudioUnitParameterUnit_Milliseconds: u32 = 24;
pub const kAudioUnitParameterUnit_CustomUnit: u32 = 26;

// Stream formats.
pub const kAudioFormatLinearPCM: u32 = fourcc(b"lpcm");
/// Float | Packed | NonInterleaved (native endian).
pub const kAudioFormatFlags_FloatNonInterleaved: u32 = 1 | 8 | 32;
pub const kAudioTimeStampSampleTimeValid: u32 = 1;

// Scheduled parameter events.
pub const kParameterEvent_Immediate: u32 = 1;

// Parameter listener events.
pub const kAudioUnitEvent_ParameterValueChange: u32 = 0;
pub const kAudioUnitEvent_BeginParameterChangeGesture: u32 = 1;
pub const kAudioUnitEvent_EndParameterChangeGesture: u32 = 2;
pub const kAUParameterListener_AnyParameter: u32 = 0xFFFF_FFFF;

// CoreFoundation.
pub const kCFStringEncodingUTF8: u32 = 0x0800_0100;
pub const kCFPropertyListBinaryFormat_v1_0: CFIndex = 200;
pub const kCFPropertyListImmutable: usize = 0;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AudioComponentDescription {
    pub componentType: OSType,
    pub componentSubType: OSType,
    pub componentManufacturer: OSType,
    pub componentFlags: u32,
    pub componentFlagsMask: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AudioStreamBasicDescription {
    pub mSampleRate: f64,
    pub mFormatID: u32,
    pub mFormatFlags: u32,
    pub mBytesPerPacket: u32,
    pub mFramesPerPacket: u32,
    pub mBytesPerFrame: u32,
    pub mChannelsPerFrame: u32,
    pub mBitsPerChannel: u32,
    pub mReserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AudioBuffer {
    pub mNumberChannels: u32,
    pub mDataByteSize: u32,
    pub mData: *mut c_void,
}

/// `AudioBufferList` with room for `N` buffers (the C type is declared
/// with one and allocated larger; the layout is the same).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AudioBufferList<const N: usize> {
    pub mNumberBuffers: u32,
    pub mBuffers: [AudioBuffer; N],
}

impl<const N: usize> AudioBufferList<N> {
    pub fn new() -> Self {
        Self {
            mNumberBuffers: 0,
            mBuffers: [AudioBuffer {
                mNumberChannels: 1,
                mDataByteSize: 0,
                mData: std::ptr::null_mut(),
            }; N],
        }
    }
}

impl<const N: usize> Default for AudioBufferList<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SMPTETime {
    pub mSubframes: i16,
    pub mSubframeDivisor: i16,
    pub mCounter: u32,
    pub mType: u32,
    pub mFlags: u32,
    pub mHours: i16,
    pub mMinutes: i16,
    pub mSeconds: i16,
    pub mFrames: i16,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AudioTimeStamp {
    pub mSampleTime: f64,
    pub mHostTime: u64,
    pub mRateScalar: f64,
    pub mWordClockTime: u64,
    pub mSMPTETime: SMPTETime,
    pub mFlags: u32,
    pub mReserved: u32,
}

pub type AURenderCallback = unsafe extern "C" fn(
    in_ref_con: *mut c_void,
    io_action_flags: *mut u32,
    in_time_stamp: *const AudioTimeStamp,
    in_bus_number: u32,
    in_number_frames: u32,
    io_data: *mut AudioBufferList<1>,
) -> OSStatus;

#[repr(C)]
pub struct AURenderCallbackStruct {
    pub inputProc: Option<AURenderCallback>,
    pub inputProcRefCon: *mut c_void,
}

pub type HostCallback_GetBeatAndTempo =
    unsafe extern "C" fn(user: *mut c_void, beat: *mut f64, tempo: *mut f64) -> OSStatus;
pub type HostCallback_GetMusicalTimeLocation = unsafe extern "C" fn(
    user: *mut c_void,
    delta_to_next_beat: *mut u32,
    numerator: *mut f32,
    denominator: *mut u32,
    measure_down_beat: *mut f64,
) -> OSStatus;
pub type HostCallback_GetTransportState = unsafe extern "C" fn(
    user: *mut c_void,
    playing: *mut Boolean,
    changed: *mut Boolean,
    sample: *mut f64,
    cycling: *mut Boolean,
    cycle_start: *mut f64,
    cycle_end: *mut f64,
) -> OSStatus;
pub type HostCallback_GetTransportState2 = unsafe extern "C" fn(
    user: *mut c_void,
    playing: *mut Boolean,
    recording: *mut Boolean,
    changed: *mut Boolean,
    sample: *mut f64,
    cycling: *mut Boolean,
    cycle_start: *mut f64,
    cycle_end: *mut f64,
) -> OSStatus;

#[repr(C)]
pub struct HostCallbackInfo {
    pub hostUserData: *mut c_void,
    pub beatAndTempoProc: Option<HostCallback_GetBeatAndTempo>,
    pub musicalTimeLocationProc: Option<HostCallback_GetMusicalTimeLocation>,
    pub transportStateProc: Option<HostCallback_GetTransportState>,
    pub transportStateProc2: Option<HostCallback_GetTransportState2>,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AudioUnitParameterInfo {
    pub name: [c_char; 52],
    pub unitName: CFStringRef,
    pub clumpID: u32,
    pub cfNameString: CFStringRef,
    pub unit: u32,
    pub minValue: f32,
    pub maxValue: f32,
    pub defaultValue: f32,
    pub flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AudioUnitParameterStringFromValue {
    pub inParamID: u32,
    pub inValue: *const f32,
    pub outString: CFStringRef,
}

/// `AudioUnitParameterEvent` with the immediate variant of its union (the
/// padding keeps the size of the larger ramp variant).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AudioUnitParameterEvent {
    pub scope: u32,
    pub element: u32,
    pub parameter: u32,
    pub eventType: u32,
    pub bufferOffset: u32,
    pub value: f32,
    pub _ramp_rest: [u32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AudioUnitParameter {
    pub mAudioUnit: AudioUnit,
    pub mParameterID: u32,
    pub mScope: u32,
    pub mElement: u32,
}

/// `AudioUnitEvent` with the parameter variant of its union (the property
/// variant has the same size).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AudioUnitEvent {
    pub mEventType: u32,
    pub mParameter: AudioUnitParameter,
}

pub type AudioUnitPropertyListenerProc =
    unsafe extern "C" fn(user: *mut c_void, unit: AudioUnit, id: u32, scope: u32, element: u32);

pub type AUEventListenerProc = unsafe extern "C" fn(
    ref_con: *mut c_void,
    object: *mut c_void,
    event: *const AudioUnitEvent,
    host_time: u64,
    value: f32,
);

#[link(name = "AudioToolbox", kind = "framework")]
unsafe extern "C" {
    pub fn AudioComponentFindNext(
        prev: AudioComponent,
        desc: *const AudioComponentDescription,
    ) -> AudioComponent;
    pub fn AudioComponentCopyName(c: AudioComponent, name: *mut CFStringRef) -> OSStatus;
    pub fn AudioComponentGetDescription(
        c: AudioComponent,
        desc: *mut AudioComponentDescription,
    ) -> OSStatus;
    pub fn AudioComponentGetVersion(c: AudioComponent, version: *mut u32) -> OSStatus;
    pub fn AudioComponentInstanceNew(c: AudioComponent, out: *mut AudioUnit) -> OSStatus;
    pub fn AudioComponentInstanceDispose(unit: AudioUnit) -> OSStatus;

    pub fn AudioUnitInitialize(unit: AudioUnit) -> OSStatus;
    pub fn AudioUnitUninitialize(unit: AudioUnit) -> OSStatus;
    pub fn AudioUnitGetPropertyInfo(
        unit: AudioUnit,
        id: u32,
        scope: u32,
        element: u32,
        size: *mut u32,
        writable: *mut Boolean,
    ) -> OSStatus;
    pub fn AudioUnitGetProperty(
        unit: AudioUnit,
        id: u32,
        scope: u32,
        element: u32,
        data: *mut c_void,
        size: *mut u32,
    ) -> OSStatus;
    pub fn AudioUnitSetProperty(
        unit: AudioUnit,
        id: u32,
        scope: u32,
        element: u32,
        data: *const c_void,
        size: u32,
    ) -> OSStatus;
    pub fn AudioUnitAddPropertyListener(
        unit: AudioUnit,
        id: u32,
        proc_: AudioUnitPropertyListenerProc,
        user: *mut c_void,
    ) -> OSStatus;
    pub fn AudioUnitRemovePropertyListenerWithUserData(
        unit: AudioUnit,
        id: u32,
        proc_: AudioUnitPropertyListenerProc,
        user: *mut c_void,
    ) -> OSStatus;
    pub fn AudioUnitGetParameter(
        unit: AudioUnit,
        id: u32,
        scope: u32,
        element: u32,
        value: *mut f32,
    ) -> OSStatus;
    pub fn AudioUnitSetParameter(
        unit: AudioUnit,
        id: u32,
        scope: u32,
        element: u32,
        value: f32,
        buffer_offset: u32,
    ) -> OSStatus;
    pub fn AudioUnitScheduleParameters(
        unit: AudioUnit,
        events: *const AudioUnitParameterEvent,
        count: u32,
    ) -> OSStatus;
    pub fn AudioUnitRender(
        unit: AudioUnit,
        flags: *mut u32,
        time_stamp: *const AudioTimeStamp,
        bus: u32,
        frames: u32,
        data: *mut AudioBufferList<1>,
    ) -> OSStatus;
    pub fn AudioUnitReset(unit: AudioUnit, scope: u32, element: u32) -> OSStatus;
    pub fn MusicDeviceMIDIEvent(
        unit: AudioUnit,
        status: u32,
        data1: u32,
        data2: u32,
        offset: u32,
    ) -> OSStatus;
    /// A SysEx message (`F0 … F7`), taken before the next render.
    pub fn MusicDeviceSysEx(unit: AudioUnit, data: *const u8, length: u32) -> OSStatus;

    pub fn AUEventListenerCreate(
        proc_: AUEventListenerProc,
        user: *mut c_void,
        run_loop: CFRunLoopRef,
        run_loop_mode: CFStringRef,
        notification_interval: f32,
        value_change_granularity: f32,
        out: *mut AUEventListenerRef,
    ) -> OSStatus;
    pub fn AUEventListenerAddEventType(
        listener: AUEventListenerRef,
        object: *mut c_void,
        event: *const AudioUnitEvent,
    ) -> OSStatus;
    pub fn AUListenerDispose(listener: AUEventListenerRef) -> OSStatus;
    pub fn AUParameterListenerNotify(
        sender: *mut c_void,
        sending_object: *mut c_void,
        parameter: *const AudioUnitParameter,
    ) -> OSStatus;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    pub static kCFRunLoopDefaultMode: CFStringRef;

    pub fn CFRelease(cf: CFTypeRef);
    pub fn CFRunLoopGetMain() -> CFRunLoopRef;
    pub fn CFStringGetLength(s: CFStringRef) -> CFIndex;
    pub fn CFStringGetMaximumSizeForEncoding(length: CFIndex, encoding: u32) -> CFIndex;
    pub fn CFStringGetCString(
        s: CFStringRef,
        buffer: *mut c_char,
        size: CFIndex,
        encoding: u32,
    ) -> Boolean;
    pub fn CFDataCreate(alloc: CFAllocatorRef, bytes: *const u8, length: CFIndex) -> CFDataRef;
    pub fn CFDataGetLength(data: CFDataRef) -> CFIndex;
    pub fn CFDataGetBytePtr(data: CFDataRef) -> *const u8;
    pub fn CFPropertyListCreateData(
        alloc: CFAllocatorRef,
        plist: CFPropertyListRef,
        format: CFIndex,
        options: usize,
        error: *mut CFTypeRef,
    ) -> CFDataRef;
    pub fn CFPropertyListCreateWithData(
        alloc: CFAllocatorRef,
        data: CFDataRef,
        options: usize,
        format: *mut CFIndex,
        error: *mut CFTypeRef,
    ) -> CFPropertyListRef;
}

/// Release a CoreFoundation object if there is one.
pub fn release(cf: CFTypeRef) {
    if !cf.is_null() {
        // SAFETY: the caller owns this reference (a Copy/Create result).
        unsafe { CFRelease(cf) };
    }
}

/// A CFString's text (empty for null).
pub fn cf_string(s: CFStringRef) -> String {
    if s.is_null() {
        return String::new();
    }
    // SAFETY: `s` is a live CFString; the buffer is sized for its UTF-8
    // form plus the terminator.
    unsafe {
        let max =
            CFStringGetMaximumSizeForEncoding(CFStringGetLength(s), kCFStringEncodingUTF8) + 1;
        let mut buf = vec![0u8; max.max(1) as usize];
        if CFStringGetCString(s, buf.as_mut_ptr().cast(), max, kCFStringEncodingUTF8) == 0 {
            return String::new();
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..end]).into_owned()
    }
}

/// A CFData's bytes.
pub fn cf_data(d: CFDataRef) -> Vec<u8> {
    if d.is_null() {
        return Vec::new();
    }
    // SAFETY: `d` is a live CFData; its pointer is valid for its length.
    unsafe {
        let len = CFDataGetLength(d).max(0) as usize;
        let ptr = CFDataGetBytePtr(d);
        if ptr.is_null() || len == 0 {
            return Vec::new();
        }
        std::slice::from_raw_parts(ptr, len).to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_match_the_c_headers() {
        use std::mem::{offset_of, size_of};
        assert_eq!(size_of::<AudioComponentDescription>(), 20);
        assert_eq!(size_of::<AudioStreamBasicDescription>(), 40);
        assert_eq!(size_of::<AudioBuffer>(), 16);
        assert_eq!(offset_of!(AudioBufferList<1>, mBuffers), 8);
        assert_eq!(size_of::<AudioBufferList<1>>(), 24);
        assert_eq!(size_of::<SMPTETime>(), 24);
        assert_eq!(size_of::<AudioTimeStamp>(), 64);
        assert_eq!(offset_of!(AudioTimeStamp, mFlags), 56);
        assert_eq!(size_of::<AudioUnitParameterInfo>(), 104);
        assert_eq!(offset_of!(AudioUnitParameterInfo, unitName), 56);
        assert_eq!(offset_of!(AudioUnitParameterInfo, flags), 96);
        assert_eq!(size_of::<AudioUnitParameterEvent>(), 32);
        assert_eq!(offset_of!(AudioUnitParameterEvent, bufferOffset), 16);
        assert_eq!(size_of::<AudioUnitEvent>(), 32);
        assert_eq!(size_of::<HostCallbackInfo>(), 40);
        assert_eq!(fourcc(b"aufx"), 0x6175_6678);
    }
}
