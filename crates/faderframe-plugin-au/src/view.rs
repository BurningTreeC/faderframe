//! An Audio Unit's editor view: the unit's own Cocoa UI
//! (`kAudioUnitProperty_CocoaUI`: a view factory class in a bundle) or, for
//! units without one, CoreAudioKit's generic parameter view. The host adds
//! the view to its window's content view.

use crate::ffi::*;
use objc2::encode::{Encode, Encoding};
use objc2::msg_send;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject};
use std::ffi::c_void;

#[link(name = "CoreAudioKit", kind = "framework")]
unsafe extern "C" {}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Size {
    pub width: f64,
    pub height: f64,
}

// SAFETY: the layout and encoding of CGSize.
unsafe impl Encode for Size {
    const ENCODING: Encoding = Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Point {
    pub x: f64,
    pub y: f64,
}

// SAFETY: the layout and encoding of CGPoint.
unsafe impl Encode for Point {
    const ENCODING: Encoding = Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Rect {
    pub origin: Point,
    pub size: Size,
}

// SAFETY: the layout and encoding of CGRect.
unsafe impl Encode for Rect {
    const ENCODING: Encoding = Encoding::Struct("CGRect", &[Point::ENCODING, Size::ENCODING]);
}

/// The unit's own view, from its Cocoa UI bundle.
fn cocoa_ui(unit: AudioUnit) -> Option<Retained<AnyObject>> {
    let mut size = 0u32;
    let mut writable = 0u8;
    // SAFETY: plain property query.
    let ok = unsafe {
        AudioUnitGetPropertyInfo(
            unit,
            kAudioUnitProperty_CocoaUI,
            kAudioUnitScope_Global,
            0,
            &mut size,
            &mut writable,
        )
    } == 0;
    let words = size as usize / std::mem::size_of::<usize>();
    if !ok || words < 2 {
        return None;
    }
    // AudioUnitCocoaViewInfo: a bundle URL, then class names.
    let mut info = vec![0usize; words];
    // SAFETY: `info` has room for `size` bytes.
    let ok = unsafe {
        AudioUnitGetProperty(
            unit,
            kAudioUnitProperty_CocoaUI,
            kAudioUnitScope_Global,
            0,
            info.as_mut_ptr().cast(),
            &mut size,
        )
    } == 0;
    if !ok {
        return None;
    }
    let url = info[0] as CFURLRef;
    let class_name = info[1] as CFStringRef;
    // SAFETY: CFURL/CFString are toll-free bridged to NSURL/NSString; the
    // factory class conforms to AUCocoaUIBase.
    let view = unsafe {
        let bundle: *mut AnyObject =
            msg_send![AnyClass::get(c"NSBundle")?, bundleWithURL: url as *mut AnyObject];
        let class: *const AnyClass = if bundle.is_null() {
            std::ptr::null()
        } else {
            msg_send![bundle, classNamed: class_name as *mut AnyObject]
        };
        match class.as_ref() {
            Some(class) => {
                let factory: Option<Retained<AnyObject>> = msg_send![class, new];
                factory.and_then(|f| {
                    let v: Option<Retained<AnyObject>> = msg_send![
                        &*f,
                        uiViewForAudioUnit: unit,
                        withSize: Size::default()
                    ];
                    v
                })
            }
            None => None,
        }
    };
    // The URL and every class name belong to us now.
    let filled = (size as usize / std::mem::size_of::<usize>()).min(words);
    for &w in &info[..filled] {
        release(w as *const c_void);
    }
    view
}

/// CoreAudioKit's generic parameter view.
fn generic_view(unit: AudioUnit) -> Option<Retained<AnyObject>> {
    let class = AnyClass::get(c"AUGenericView")?;
    // SAFETY: AUGenericView's designated initialiser for a unit.
    unsafe {
        let obj: Allocated<AnyObject> = msg_send![class, alloc];
        msg_send![obj, initWithAudioUnit: unit]
    }
}

/// The editor view: the unit's own, else the generic one.
pub(crate) fn create(unit: AudioUnit) -> Option<Retained<AnyObject>> {
    cocoa_ui(unit).or_else(|| generic_view(unit))
}

pub(crate) fn has_generic() -> bool {
    AnyClass::get(c"AUGenericView").is_some()
}

pub(crate) fn size_of(view: &AnyObject) -> (u32, u32) {
    // SAFETY: an NSView.
    let frame: Rect = unsafe { msg_send![view, frame] };
    (
        frame.size.width.round().max(1.0) as u32,
        frame.size.height.round().max(1.0) as u32,
    )
}

pub(crate) fn set_size(view: &AnyObject, (w, h): (u32, u32)) {
    let size = Size {
        width: f64::from(w),
        height: f64::from(h),
    };
    // SAFETY: an NSView.
    unsafe {
        let _: () = msg_send![view, setFrameSize: size];
    }
}

/// Put `view` into the host's content view (an `NSView` pointer).
pub(crate) fn attach(view: &AnyObject, parent: u64) -> bool {
    let parent = parent as usize as *mut AnyObject;
    if parent.is_null() {
        return false;
    }
    // SAFETY: the host passes a live NSView; the view is ours.
    unsafe {
        let origin = Point::default();
        let _: () = msg_send![view, setFrameOrigin: origin];
        let _: () = msg_send![parent, addSubview: view];
    }
    true
}

pub(crate) fn detach(view: &AnyObject) {
    // SAFETY: an NSView (removing a view without a superview is a no-op).
    unsafe {
        let _: () = msg_send![view, removeFromSuperview];
    }
}
