//! NVMe health through Apple's SMART plugin. Works without root, tested on the internal SSD.
//! Drives behind USB bridges usually don't expose this, so they report no health at all.

use crate::iokit::{self, Entry};
use core_foundation_sys::uuid::{CFUUIDBytes, CFUUIDGetConstantUUIDWithBytes, CFUUIDGetUUIDBytes, CFUUIDRef};
use serde::Serialize;
use std::ffi::c_void;

const LOG_SIZE: usize = 512;
const DATA_UNIT_BYTES: u128 = 512_000;
const KELVIN_OFFSET: i32 = 273;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Health {
    pub critical_warning: u8,
    pub temperature_c: i32,
    pub available_spare_percent: u8,
    pub percent_used: u8,
    pub bytes_read: u128,
    pub bytes_written: u128,
    pub power_on_hours: u128,
    pub power_cycles: u128,
    pub unsafe_shutdowns: u128,
    pub media_errors: u128,
}

fn u128_at(log: &[u8; LOG_SIZE], offset: usize) -> u128 {
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&log[offset..offset + 16]);
    u128::from_le_bytes(bytes)
}

/// Field offsets are from the NVMe spec's SMART / Health Information log page.
pub fn parse_log(log: &[u8; LOG_SIZE]) -> Health {
    Health {
        critical_warning: log[0],
        temperature_c: i32::from(u16::from_le_bytes([log[1], log[2]])) - KELVIN_OFFSET,
        available_spare_percent: log[3],
        percent_used: log[5],
        bytes_read: u128_at(log, 32).saturating_mul(DATA_UNIT_BYTES),
        bytes_written: u128_at(log, 48).saturating_mul(DATA_UNIT_BYTES),
        power_cycles: u128_at(log, 112),
        power_on_hours: u128_at(log, 128),
        unsafe_shutdowns: u128_at(log, 144),
        media_errors: u128_at(log, 160),
    }
}

type Release = unsafe extern "C" fn(*mut c_void) -> u32;
type Query = unsafe extern "C" fn(*mut c_void, CFUUIDBytes, *mut *mut c_void) -> i32;

// Layouts mirror IOCFPlugIn.h and NVMeSMARTLibExternal.h; only the leading fields are declared.
#[repr(C)]
struct PlugInVtable {
    _reserved: *mut c_void,
    query_interface: Query,
    _add_ref: *mut c_void,
    release: Release,
}

#[repr(C)]
struct SmartVtable {
    _reserved: *mut c_void,
    _query_interface: *mut c_void,
    _add_ref: *mut c_void,
    release: Release,
    _version: u16,
    _revision: u16,
    smart_read_data: unsafe extern "C" fn(*mut c_void, *mut [u8; LOG_SIZE]) -> i32,
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOCreatePlugInInterfaceForService(
        service: u32,
        plugin_type: CFUUIDRef,
        interface_type: CFUUIDRef,
        interface: *mut *mut *mut PlugInVtable,
        score: *mut i32,
    ) -> i32;
    fn IODestroyPlugInInterface(interface: *mut *mut PlugInVtable) -> i32;
}

#[allow(clippy::too_many_arguments)]
fn uuid(b: [u8; 16]) -> CFUUIDRef {
    // SAFETY: constant UUIDs are interned by CoreFoundation and never freed.
    unsafe {
        CFUUIDGetConstantUUIDWithBytes(
            std::ptr::null(),
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
            b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15],
        )
    }
}

fn read_log(entry: &Entry) -> Option<[u8; LOG_SIZE]> {
    let client_type = uuid([0xAA, 0x0F, 0xA6, 0xF9, 0xC2, 0xD6, 0x45, 0x7F, 0xB1, 0x0B, 0x59, 0xA1, 0x32, 0x53, 0x29, 0x2F]);
    let plugin_interface = uuid([0xC2, 0x44, 0xE8, 0x58, 0x10, 0x9C, 0x11, 0xD4, 0x91, 0xD4, 0x00, 0x50, 0xE4, 0xC6, 0x42, 0x6F]);
    let smart_interface = uuid([0xCC, 0xD1, 0xDB, 0x19, 0xFD, 0x9A, 0x4D, 0xAF, 0xBF, 0x95, 0x12, 0x45, 0x4B, 0x23, 0x0A, 0xB6]);
    let mut plugin: *mut *mut PlugInVtable = std::ptr::null_mut();
    let mut score = 0;
    // SAFETY: out pointers are valid. Vtable layouts match the SDK headers, and every
    // interface obtained here is released before returning.
    unsafe {
        let code = IOCreatePlugInInterfaceForService(entry.0, client_type, plugin_interface, &mut plugin, &mut score);
        if code != 0 || plugin.is_null() {
            return None;
        }
        let mut smart: *mut c_void = std::ptr::null_mut();
        let result = ((**plugin).query_interface)(plugin.cast(), CFUUIDGetUUIDBytes(smart_interface), &mut smart);
        let mut log = None;
        if result == 0 && !smart.is_null() {
            let vtable = smart.cast::<*mut SmartVtable>();
            let mut buffer = [0u8; LOG_SIZE];
            if ((**vtable).smart_read_data)(smart, &mut buffer) == 0 {
                log = Some(buffer);
            }
            ((**vtable).release)(smart);
        }
        IODestroyPlugInInterface(plugin);
        log
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DriveHealth {
    pub bsd_name: Option<String>,
    pub model: Option<String>,
    pub health: Health,
}

pub fn read_all() -> Vec<DriveHealth> {
    iokit::services_with_true_property("NVMe SMART Capable")
        .iter()
        .filter_map(|entry| {
            let health = parse_log(&read_log(entry)?);
            Some(DriveHealth {
                bsd_name: entry
                    .search("BSD Name", iokit::SEARCH_RECURSIVE)
                    .and_then(|v| iokit::as_string(&v)),
                model: entry
                    .search("Model Number", iokit::SEARCH_RECURSIVE | iokit::SEARCH_PARENTS)
                    .and_then(|v| iokit::as_string(&v))
                    .map(|s| s.trim().to_string()),
                health,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_nvme_log_layout() {
        let mut log = [0u8; LOG_SIZE];
        log[0] = 2;
        log[1..3].copy_from_slice(&309u16.to_le_bytes());
        log[3] = 99;
        log[5] = 3;
        log[32] = 10;
        log[48] = 1;
        log[49] = 1;
        log[112] = 7;
        log[128] = 8;
        log[144] = 9;
        log[160] = 11;
        let health = parse_log(&log);
        assert_eq!(health.critical_warning, 2);
        assert_eq!(health.temperature_c, 36);
        assert_eq!(health.available_spare_percent, 99);
        assert_eq!(health.percent_used, 3);
        assert_eq!(health.bytes_read, 5_120_000);
        assert_eq!(health.bytes_written, 257 * 512_000);
        assert_eq!(health.power_cycles, 7);
        assert_eq!(health.power_on_hours, 8);
        assert_eq!(health.unsafe_shutdowns, 9);
        assert_eq!(health.media_errors, 11);
    }

    #[test]
    fn any_nvme_drive_reports_sensible_health() {
        for drive in read_all() {
            assert!((0..120).contains(&drive.health.temperature_c));
        }
    }
}
