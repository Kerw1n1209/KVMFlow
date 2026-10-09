//! macOS USB adapter - IOKit snapshot polling. Port of the KVM-1 probe's
//! enumeration (probes/mac/Sources/kvmprobe/Usb.swift) with one deliberate
//! change: the product polls snapshots instead of registering matching
//! notifications. The trigger engine is snapshot-driven anyway (duplicate
//! macOS disconnect notifications must be collapsed), and 250ms polling is
//! two orders of magnitude inside the 5s/10s debounce constants.

use crate::backends::UsbDevice;
use core_foundation::base::TCFType;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    static kIOMainPortDefault: u32;
    fn IOServiceMatching(name: *const std::ffi::c_char) -> *mut std::ffi::c_void;
    fn IOServiceGetMatchingServices(
        port: u32,
        matching: *mut std::ffi::c_void,
        existing: *mut u32,
    ) -> i32;
    fn IOIteratorNext(iterator: u32) -> u32;
    fn IOObjectRelease(obj: u32) -> i32;
    fn IORegistryEntryCreateCFProperty(
        entry: u32,
        key: core_foundation::string::CFStringRef,
        allocator: *const std::ffi::c_void,
        options: u32,
    ) -> *const std::ffi::c_void;
    fn IORegistryEntryGetRegistryEntryID(entry: u32, id: *mut u64) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFAllocatorDefault: *const std::ffi::c_void;
}

const USB_DEVICE_CLASSES: [&str; 2] = ["IOUSBDevice", "IOUSBHostDevice"];

fn str_prop(service: u32, name: &str) -> Option<String> {
    let key = core_foundation::string::CFString::new(name);
    let cf = unsafe {
        IORegistryEntryCreateCFProperty(service, key.as_concrete_TypeRef(), kCFAllocatorDefault, 0)
    };
    if cf.is_null() {
        return None;
    }
    let typed = unsafe { core_foundation::base::CFType::wrap_under_create_rule(cf) };
    typed
        .downcast_into::<core_foundation::string::CFString>()
        .map(|s| s.to_string())
}

fn num_prop(service: u32, name: &str) -> Option<i64> {
    let key = core_foundation::string::CFString::new(name);
    let cf = unsafe {
        IORegistryEntryCreateCFProperty(service, key.as_concrete_TypeRef(), kCFAllocatorDefault, 0)
    };
    if cf.is_null() {
        return None;
    }
    let typed = unsafe { core_foundation::base::CFType::wrap_under_create_rule(cf) };
    if typed.type_of() == core_foundation::number::CFNumber::type_id() {
        return typed
            .downcast_into::<core_foundation::number::CFNumber>()
            .and_then(|n| n.to_i64());
    }
    if typed.type_of() == core_foundation::string::CFString::type_id() {
        return typed
            .downcast_into::<core_foundation::string::CFString>()
            .and_then(|s| s.to_string().parse().ok());
    }
    None
}

/// Enumerate all USB devices visible to IOKit, deduped by registry entry ID
/// (same rule as the KVM-1 probe snapshots).
pub fn usb_snapshot() -> Vec<UsbDevice> {
    let mut seen = std::collections::HashSet::new();
    let mut devices = Vec::new();

    for class in USB_DEVICE_CLASSES {
        let cname = std::ffi::CString::new(class).unwrap();
        let matching = unsafe { IOServiceMatching(cname.as_ptr()) };
        if matching.is_null() {
            continue;
        }
        let mut iterator: u32 = 0;
        let kr =
            unsafe { IOServiceGetMatchingServices(kIOMainPortDefault, matching, &mut iterator) };
        if kr != 0 {
            continue;
        }
        loop {
            let service = unsafe { IOIteratorNext(iterator) };
            if service == 0 {
                break;
            }
            let mut rid: u64 = 0;
            unsafe { IORegistryEntryGetRegistryEntryID(service, &mut rid) };
            if !seen.insert(rid) {
                unsafe { IOObjectRelease(service) };
                continue;
            }
            let vid = num_prop(service, "idVendor").unwrap_or(-1);
            let pid = num_prop(service, "idProduct").unwrap_or(-1);
            if vid < 0 || pid < 0 {
                unsafe { IOObjectRelease(service) };
                continue;
            }
            let serial = str_prop(service, "USB Serial Number").unwrap_or_default();
            let device = UsbDevice {
                key: format!("{vid:04x}:{pid:04x}:{serial}"),
                vid_pid: format!("{vid:04x}:{pid:04x}"),
                serial,
                product: str_prop(service, "USB Product Name").unwrap_or_default(),
                vendor: str_prop(service, "USB Vendor Name").unwrap_or_default(),
            };
            devices.push(device);
            unsafe { IOObjectRelease(service) };
        }
        unsafe { IOObjectRelease(iterator) };
    }

    devices.sort_by(|a, b| a.key.cmp(&b.key));
    devices
}
