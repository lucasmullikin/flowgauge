//! Memory breakdown from the Mach VM statistics, matching how Activity Monitor groups pages.

use serde::Serialize;
use std::ffi::CString;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pages {
    pub internal: u64,
    pub purgeable: u64,
    pub wired: u64,
    pub compressed: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Memory {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub app_bytes: u64,
    pub wired_bytes: u64,
    pub compressed_bytes: u64,
    pub cached_files_bytes: u64,
    pub free_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
    pub pressure: Pressure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Pressure {
    Normal,
    Warning,
    Critical,
    Unknown,
}

pub fn pressure_from_level(level: i32) -> Pressure {
    match level {
        1 => Pressure::Normal,
        2 => Pressure::Warning,
        4 => Pressure::Critical,
        _ => Pressure::Unknown,
    }
}

/// Returns (app, wired, compressed, used) in bytes.
pub fn breakdown(pages: Pages, page_size: u64) -> (u64, u64, u64, u64) {
    let app = pages.internal.saturating_sub(pages.purgeable) * page_size;
    let wired = pages.wired * page_size;
    let compressed = pages.compressed * page_size;
    (app, wired, compressed, app + wired + compressed)
}

fn sysctl_value<T: Default + Copy>(name: &str) -> Option<T> {
    let name = CString::new(name).ok()?;
    let mut value = T::default();
    let mut size = std::mem::size_of::<T>();
    // SAFETY: value has `size` writable bytes, and the kernel reports what it wrote.
    let code = unsafe {
        libc::sysctlbyname(name.as_ptr(), (&raw mut value).cast(), &mut size, std::ptr::null_mut(), 0)
    };
    (code == 0 && size == std::mem::size_of::<T>()).then_some(value)
}

pub fn read() -> Option<Memory> {
    let mut stats: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
    let mut count = libc::HOST_VM_INFO64_COUNT;
    // SAFETY: stats and count describe a correctly sized vm_statistics64.
    #[allow(deprecated)]
    let code = unsafe {
        libc::host_statistics64(
            libc::mach_host_self(),
            libc::HOST_VM_INFO64,
            (&raw mut stats).cast(),
            &mut count,
        )
    };
    if code != 0 {
        return None;
    }
    // SAFETY: sysconf has no preconditions.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
    let pages = Pages {
        internal: u64::from(stats.internal_page_count),
        purgeable: u64::from(stats.purgeable_count),
        wired: u64::from(stats.wire_count),
        compressed: u64::from(stats.compressor_page_count),
    };
    let (app_bytes, wired_bytes, compressed_bytes, used_bytes) = breakdown(pages, page_size);
    let cached_pages = u64::from(stats.external_page_count) + pages.purgeable;
    let free_pages = u64::from(stats.free_count);
    let swap: libc::xsw_usage = {
        let mut value: libc::xsw_usage = unsafe { std::mem::zeroed() };
        let name = CString::new("vm.swapusage").ok()?;
        let mut size = std::mem::size_of::<libc::xsw_usage>();
        // SAFETY: value has `size` writable bytes.
        let code = unsafe {
            libc::sysctlbyname(name.as_ptr(), (&raw mut value).cast(), &mut size, std::ptr::null_mut(), 0)
        };
        // A failed or short read would otherwise show as no swap in use.
        if code != 0 || size != std::mem::size_of::<libc::xsw_usage>() {
            return None;
        }
        value
    };
    Some(Memory {
        total_bytes: sysctl_value::<u64>("hw.memsize")?,
        used_bytes,
        app_bytes,
        wired_bytes,
        compressed_bytes,
        cached_files_bytes: cached_pages * page_size,
        free_bytes: free_pages * page_size,
        swap_total_bytes: swap.xsu_total,
        swap_used_bytes: swap.xsu_used,
        pressure: sysctl_value::<i32>("kern.memorystatus_vm_pressure_level")
            .map_or(Pressure::Unknown, pressure_from_level),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_memory_excludes_purgeable_pages() {
        let pages = Pages { internal: 100, purgeable: 30, wired: 10, compressed: 5 };
        assert_eq!(breakdown(pages, 16384), (70 * 16384, 10 * 16384, 5 * 16384, 85 * 16384));
    }

    #[test]
    fn purgeable_above_internal_does_not_underflow() {
        let pages = Pages { internal: 5, purgeable: 30, ..Pages::default() };
        assert_eq!(breakdown(pages, 16384).0, 0);
    }

    #[test]
    fn maps_pressure_levels() {
        assert_eq!(pressure_from_level(1), Pressure::Normal);
        assert_eq!(pressure_from_level(2), Pressure::Warning);
        assert_eq!(pressure_from_level(4), Pressure::Critical);
        assert_eq!(pressure_from_level(9), Pressure::Unknown);
    }

    #[test]
    fn reads_real_memory() {
        let memory = read().unwrap();
        assert!(memory.total_bytes > 0);
        assert!(memory.used_bytes > 0 && memory.used_bytes <= memory.total_bytes);
    }
}
