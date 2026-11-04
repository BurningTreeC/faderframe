//! ARA through VST3 (`ARAVST3.h`): the component's `ARA::IPlugInEntryPoint`
//! (its factory) and `IPlugInEntryPoint2` (binding with roles), and the
//! module's "ARA Main Factory Class" classes that mark ARA plugins.

use std::ffi::c_void;
use vst3::ComPtr;
use vst3::Steinberg::Vst::IComponent;
use vst3::Steinberg::{FUnknown, FUnknownVtbl, TUID, kResultOk};

/// The class category of `ARA::IMainFactory` classes (named like the
/// plugin's audio processor class).
pub const MAIN_FACTORY_CLASS: &str = "ARA Main Factory Class";

const IID_ENTRY_POINT: TUID = vst3::uid(0x12814E54, 0xA1CE4076, 0x82B96813, 0x16950BD6);
const IID_ENTRY_POINT_2: TUID = vst3::uid(0xCD9A5913, 0xC9EB46D7, 0x96CA53AD, 0xD1DB89F5);

/// `ARA::IPlugInEntryPoint`.
#[repr(C)]
struct EntryPointVtbl {
    base: FUnknownVtbl,
    get_factory: unsafe extern "system" fn(this: *mut c_void) -> *const c_void,
    /// Deprecated since ARA 2 (all three roles implied).
    bind_to_document_controller:
        unsafe extern "system" fn(this: *mut c_void, controller: *mut c_void) -> *const c_void,
}

/// `ARA::IPlugInEntryPoint2`.
#[repr(C)]
struct EntryPoint2Vtbl {
    base: FUnknownVtbl,
    bind_to_document_controller_with_roles: unsafe extern "system" fn(
        this: *mut c_void,
        controller: *mut c_void,
        known: i32,
        assigned: i32,
    ) -> *const c_void,
}

#[repr(C)]
struct Object<V> {
    vtbl: *const V,
}

/// An interface of a COM object, released when dropped.
struct Queried<V> {
    obj: *mut Object<V>,
}

impl<V> Queried<V> {
    /// # Safety
    /// `iid` names an interface whose vtable starts like `V`.
    unsafe fn of(component: &ComPtr<IComponent>, iid: &TUID) -> Option<Self> {
        let unknown = component.as_ptr() as *mut FUnknown;
        let mut obj: *mut c_void = std::ptr::null_mut();
        // SAFETY: a live component; queryInterface adds a reference to
        // what it returns.
        let ok = unsafe { ((*(*unknown).vtbl).queryInterface)(unknown, iid, &mut obj) };
        // Made only on success: dropping one releases what it holds.
        if ok == kResultOk && !obj.is_null() {
            Some(Queried {
                obj: obj as *mut Object<V>,
            })
        } else {
            None
        }
    }

    fn this(&self) -> *mut c_void {
        self.obj as *mut c_void
    }

    fn vtbl(&self) -> &V {
        // SAFETY: a live interface pointer (the caller's IID contract).
        unsafe { &*(*self.obj).vtbl }
    }
}

impl<V> Drop for Queried<V> {
    fn drop(&mut self) {
        let unknown = self.obj as *mut FUnknown;
        // SAFETY: the reference queryInterface added.
        unsafe { ((*(*unknown).vtbl).release)(unknown) };
    }
}

/// The component's ARA factory (an ARA plugin's audio processor class).
pub fn factory(component: &ComPtr<IComponent>) -> Option<faderframe_ara::Factory> {
    // SAFETY: the IID of `IPlugInEntryPoint`, whose vtable `EntryPointVtbl`
    // declares.
    let entry = unsafe { Queried::<EntryPointVtbl>::of(component, &IID_ENTRY_POINT) }?;
    // SAFETY: a live interface; the factory lives as long as the module,
    // which is never unloaded.
    unsafe {
        let ptr = (entry.vtbl().get_factory)(entry.this());
        faderframe_ara::Factory::from_raw(ptr)
    }
}

/// Bind the component to an ARA document controller with `roles` (once,
/// before `setActive`, `setState`, `getProcessContextRequirements` or its
/// editor). Plug-ins without `IPlugInEntryPoint2` take ARA 1's binding,
/// which implies every role.
pub fn bind(
    component: &ComPtr<IComponent>,
    controller: faderframe_ara::ControllerRef,
    roles: faderframe_ara::Roles,
) -> Option<faderframe_ara::Extension> {
    // SAFETY: the IIDs of the two entry points with their vtables; a live
    // component and controller, control thread, called once; the extension
    // lives as long as the component, and the engine drops its copy first.
    unsafe {
        let raw = if let Some(e2) = Queried::<EntryPoint2Vtbl>::of(component, &IID_ENTRY_POINT_2) {
            (e2.vtbl().bind_to_document_controller_with_roles)(
                e2.this(),
                controller.as_raw(),
                faderframe_ara::Roles::ALL.bits(),
                roles.bits(),
            )
        } else {
            let e = Queried::<EntryPointVtbl>::of(component, &IID_ENTRY_POINT)?;
            (e.vtbl().bind_to_document_controller)(e.this(), controller.as_raw())
        };
        faderframe_ara::Extension::from_raw(raw)
    }
}

/// Whether the component's class is an ARA plugin (it answers for
/// `IPlugInEntryPoint`).
pub fn is_ara(component: &ComPtr<IComponent>) -> bool {
    // SAFETY: as in `factory`.
    unsafe { Queried::<EntryPointVtbl>::of(component, &IID_ENTRY_POINT) }.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_interface_ids_follow_the_platforms_byte_order() {
        // DECLARE_CLASS_IID (IPlugInEntryPoint, 0x12814E54, ...).
        if cfg!(windows) {
            assert_eq!(IID_ENTRY_POINT[0] as u8, 0x54);
        } else {
            assert_eq!(IID_ENTRY_POINT[0] as u8, 0x12);
            assert_eq!(IID_ENTRY_POINT_2[15] as u8, 0xF5);
        }
    }
}
