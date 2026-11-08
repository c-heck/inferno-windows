use crate::{abi::*, driver::Driver};
use std::{
    ffi::c_void,
    ptr,
    sync::atomic::{AtomicU32, Ordering},
};

static OBJECTS: AtomicU32 = AtomicU32::new(0);
static LOCKS: AtomicU32 = AtomicU32::new(0);

#[repr(C)]
struct Factory {
    vtable: &'static FactoryVtable,
    refs: AtomicU32,
}
#[repr(C)]
struct FactoryVtable {
    query: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    create:
        unsafe extern "system" fn(*mut c_void, *mut c_void, *const Guid, *mut *mut c_void) -> i32,
    lock: unsafe extern "system" fn(*mut c_void, i32) -> i32,
}

pub unsafe extern "system" fn driver_query(
    this: *mut c_void,
    iid: *const Guid,
    out: *mut *mut c_void,
) -> i32 {
    if out.is_null() {
        return E_POINTER;
    }
    unsafe {
        *out = ptr::null_mut();
    }
    if iid.is_null() {
        return E_POINTER;
    }
    if unsafe { *iid == IUNKNOWN || *iid == CLSID } {
        unsafe {
            *out = this;
            driver_add_ref(this);
        }
        OK
    } else {
        E_NOINTERFACE
    }
}
pub unsafe extern "system" fn driver_add_ref(this: *mut c_void) -> u32 {
    unsafe { &*this.cast::<Driver>() }
        .refs
        .fetch_add(1, Ordering::Relaxed)
        + 1
}
pub unsafe extern "system" fn driver_release(this: *mut c_void) -> u32 {
    if crate::engine::in_callback() {
        crate::timer::report("Release must be deferred until after the ASIO callback returns");
        return unsafe { &*this.cast::<Driver>() }
            .refs
            .load(Ordering::Acquire);
    }
    let remaining = unsafe { &*this.cast::<Driver>() }
        .refs
        .fetch_sub(1, Ordering::AcqRel)
        - 1;
    if remaining == 0 {
        unsafe {
            drop(Box::from_raw(this.cast::<Driver>()));
        }
        OBJECTS.fetch_sub(1, Ordering::AcqRel);
    }
    remaining
}
unsafe extern "system" fn factory_query(
    this: *mut c_void,
    iid: *const Guid,
    out: *mut *mut c_void,
) -> i32 {
    if out.is_null() {
        return E_POINTER;
    }
    unsafe {
        *out = ptr::null_mut();
    }
    if iid.is_null() {
        return E_POINTER;
    }
    if unsafe { *iid == IUNKNOWN || *iid == ICLASS_FACTORY } {
        unsafe {
            *out = this;
            factory_add_ref(this);
        }
        OK
    } else {
        E_NOINTERFACE
    }
}
unsafe extern "system" fn factory_add_ref(this: *mut c_void) -> u32 {
    unsafe { &*this.cast::<Factory>() }
        .refs
        .fetch_add(1, Ordering::Relaxed)
        + 1
}
unsafe extern "system" fn factory_release(this: *mut c_void) -> u32 {
    let remaining = unsafe { &*this.cast::<Factory>() }
        .refs
        .fetch_sub(1, Ordering::AcqRel)
        - 1;
    if remaining == 0 {
        unsafe {
            drop(Box::from_raw(this.cast::<Factory>()));
        }
        OBJECTS.fetch_sub(1, Ordering::AcqRel);
    }
    remaining
}
unsafe extern "system" fn factory_create(
    _: *mut c_void,
    outer: *mut c_void,
    iid: *const Guid,
    out: *mut *mut c_void,
) -> i32 {
    if out.is_null() {
        return E_POINTER;
    }
    unsafe {
        *out = ptr::null_mut();
    }
    if !outer.is_null() {
        return NO_AGGREGATION;
    }
    if iid.is_null() {
        return E_POINTER;
    }
    if unsafe { *iid != CLSID && *iid != IUNKNOWN } {
        return E_NOINTERFACE;
    }
    OBJECTS.fetch_add(1, Ordering::AcqRel);
    unsafe {
        *out = Box::into_raw(Box::new(Driver::new())).cast();
    }
    OK
}
unsafe extern "system" fn factory_lock(_: *mut c_void, lock: i32) -> i32 {
    if lock != 0 {
        LOCKS.fetch_add(1, Ordering::AcqRel);
    } else if LOCKS
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
            value.checked_sub(1)
        })
        .is_err()
    {
        return INVALID_PARAMETER;
    }
    OK
}

static FACTORY_VTABLE: FactoryVtable = FactoryVtable {
    query: factory_query,
    add_ref: factory_add_ref,
    release: factory_release,
    create: factory_create,
    lock: factory_lock,
};

/// # Safety
/// COM callers supply readable GUIDs and a writable interface output pointer.
#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    clsid: *const Guid,
    iid: *const Guid,
    out: *mut *mut c_void,
) -> i32 {
    if out.is_null() {
        return E_POINTER;
    }
    unsafe {
        *out = ptr::null_mut();
    }
    if clsid.is_null() || iid.is_null() {
        return E_POINTER;
    }
    if unsafe { *clsid != CLSID } {
        return CLASS_NOT_AVAILABLE;
    }
    if unsafe { *iid != ICLASS_FACTORY && *iid != IUNKNOWN } {
        return E_NOINTERFACE;
    }
    OBJECTS.fetch_add(1, Ordering::AcqRel);
    let factory = Box::new(Factory {
        vtable: &FACTORY_VTABLE,
        refs: AtomicU32::new(1),
    });
    unsafe {
        *out = Box::into_raw(factory).cast();
    }
    OK
}

#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> i32 {
    if OBJECTS.load(Ordering::Acquire) == 0 && LOCKS.load(Ordering::Acquire) == 0 {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn factory_interfaces_and_object_lifetimes() {
        unsafe {
            let mut factory = ptr::null_mut();
            assert_eq!(
                DllGetClassObject(&IUNKNOWN, &ICLASS_FACTORY, &mut factory),
                CLASS_NOT_AVAILABLE
            );
            assert!(factory.is_null());
            assert_eq!(DllGetClassObject(&CLSID, &ICLASS_FACTORY, &mut factory), OK);
            assert_eq!(DllCanUnloadNow(), 1);
            let mut device = ptr::null_mut();
            assert_eq!(
                factory_create(factory, factory, &CLSID, &mut device),
                NO_AGGREGATION
            );
            assert!(device.is_null());
            assert_eq!(
                factory_create(factory, ptr::null_mut(), &CLSID, &mut device),
                OK
            );
            let mut unknown = ptr::null_mut();
            assert_eq!(driver_query(device, &IUNKNOWN, &mut unknown), OK);
            assert_eq!(unknown, device);
            assert_eq!(driver_release(unknown), 1);
            assert_eq!(driver_release(device), 0);
            assert_eq!(factory_release(factory), 0);
            assert_eq!(DllCanUnloadNow(), OK);
        }
    }
}
