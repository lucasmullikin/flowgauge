//! Small safe wrapper over the IOKit registry calls the collectors share.

use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_foundation_sys::base::CFTypeRef;
use core_foundation_sys::dictionary::CFDictionaryRef;
use core_foundation_sys::string::CFStringRef;
use std::ffi::{CString, c_char, c_void};

pub const SEARCH_RECURSIVE: u32 = 1;
pub const SEARCH_PARENTS: u32 = 2;
const SERVICE_PLANE: &[u8] = b"IOService\0";

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingServices(main: u32, matching: *const c_void, iterator: *mut u32) -> i32;
    fn IOIteratorNext(iterator: u32) -> u32;
    fn IOObjectRelease(object: u32) -> i32;
    fn IORegistryEntryCreateCFProperty(
        entry: u32,
        key: CFStringRef,
        allocator: *const c_void,
        options: u32,
    ) -> CFTypeRef;
    fn IORegistryEntrySearchCFProperty(
        entry: u32,
        plane: *const c_char,
        key: CFStringRef,
        allocator: *const c_void,
        options: u32,
    ) -> CFTypeRef;
}

pub struct Entry(pub u32);

impl Drop for Entry {
    fn drop(&mut self) {
        // SAFETY: we own this registry reference.
        unsafe { IOObjectRelease(self.0) };
    }
}

/// `matching` is consumed by IOKit, as the API requires.
fn collect(matching: *const c_void) -> Vec<Entry> {
    let mut iterator = 0;
    // SAFETY: matching is a valid dictionary or null, and iterator is a valid out pointer.
    if matching.is_null() || unsafe { IOServiceGetMatchingServices(0, matching, &mut iterator) } != 0
    {
        return Vec::new();
    }
    let iterator = Entry(iterator);
    let mut entries = Vec::new();
    loop {
        // SAFETY: the iterator stays valid until `iterator` drops.
        let next = unsafe { IOIteratorNext(iterator.0) };
        if next == 0 {
            return entries;
        }
        entries.push(Entry(next));
    }
}

pub fn services_of_class(class: &str) -> Vec<Entry> {
    let Ok(name) = CString::new(class) else {
        return Vec::new();
    };
    // SAFETY: name is a valid C string for the call.
    collect(unsafe { IOServiceMatching(name.as_ptr()) })
}

pub fn services_with_true_property(property: &str) -> Vec<Entry> {
    let inner = CFDictionary::from_CFType_pairs(&[(
        CFString::new(property).as_CFType(),
        CFBoolean::true_value().as_CFType(),
    )]);
    let outer = CFDictionary::from_CFType_pairs(&[(
        CFString::new("IOPropertyMatch").as_CFType(),
        inner.as_CFType(),
    )]);
    let raw: CFDictionaryRef = outer.as_concrete_TypeRef();
    // IOKit consumes one reference, so hand it an extra one.
    std::mem::forget(outer.clone());
    collect(raw.cast())
}

impl Entry {
    pub fn property(&self, key: &str) -> Option<CFType> {
        let key = CFString::new(key);
        // SAFETY: the key outlives the call; a non-null result is ours to own.
        unsafe {
            let raw = IORegistryEntryCreateCFProperty(
                self.0,
                key.as_concrete_TypeRef(),
                std::ptr::null(),
                0,
            );
            (!raw.is_null()).then(|| CFType::wrap_under_create_rule(raw))
        }
    }

    pub fn search(&self, key: &str, options: u32) -> Option<CFType> {
        let key = CFString::new(key);
        // SAFETY: the plane and key outlive the call; a non-null result is ours to own.
        unsafe {
            let raw = IORegistryEntrySearchCFProperty(
                self.0,
                SERVICE_PLANE.as_ptr().cast(),
                key.as_concrete_TypeRef(),
                std::ptr::null(),
                options,
            );
            (!raw.is_null()).then(|| CFType::wrap_under_create_rule(raw))
        }
    }

    pub fn i64(&self, key: &str) -> Option<i64> {
        as_i64(&self.property(key)?)
    }

    pub fn bool(&self, key: &str) -> Option<bool> {
        as_bool(&self.property(key)?)
    }
}

pub fn as_i64(value: &CFType) -> Option<i64> {
    value.downcast::<CFNumber>()?.to_i64()
}

pub fn as_bool(value: &CFType) -> Option<bool> {
    value.downcast::<CFBoolean>().map(bool::from)
}

pub fn as_string(value: &CFType) -> Option<String> {
    value.downcast::<CFString>().map(|s| s.to_string())
}

pub fn dict_i64(value: &CFType, key: &str) -> Option<i64> {
    let dict: CFDictionary<CFString, CFType> =
        // SAFETY: only used after confirming the value is a dictionary.
        unsafe { CFDictionary::wrap_under_get_rule(dict_ref(value)?) };
    as_i64(&*dict.find(CFString::new(key))?)
}

pub fn dict_bool(value: &CFType, key: &str) -> Option<bool> {
    let dict: CFDictionary<CFString, CFType> =
        // SAFETY: only used after confirming the value is a dictionary.
        unsafe { CFDictionary::wrap_under_get_rule(dict_ref(value)?) };
    as_bool(&*dict.find(CFString::new(key))?)
}

pub fn dict_string(value: &CFType, key: &str) -> Option<String> {
    let dict: CFDictionary<CFString, CFType> =
        // SAFETY: only used after confirming the value is a dictionary.
        unsafe { CFDictionary::wrap_under_get_rule(dict_ref(value)?) };
    as_string(&*dict.find(CFString::new(key))?)
}

/// Each entry of an array property, for reading lists of dictionaries.
pub fn array_items(value: &CFType) -> Vec<CFType> {
    let Some(array) = value.downcast::<core_foundation::array::CFArray>() else {
        return Vec::new();
    };
    array
        .get_all_values()
        .into_iter()
        .filter(|item| !item.is_null())
        // SAFETY: a CFArray's entries are CF objects it keeps alive, and this retains each one.
        .map(|item| unsafe { CFType::wrap_under_get_rule(item) })
        .collect()
}

fn dict_ref(value: &CFType) -> Option<CFDictionaryRef> {
    value
        .instance_of::<CFDictionary>()
        .then(|| value.as_CFTypeRef().cast())
}
