//! Volumes and per-disk activity. Volumes come from the kernel's cached mount table, so a dead
//! network share can never hang a read.

use crate::iokit::{self, as_string, dict_i64};
use serde::Serialize;
use std::ffi::CStr;

const NETWORK_FILESYSTEMS: [&str; 5] = ["smbfs", "nfs", "afpfs", "webdav", "cifs"];
const HIDDEN_FILESYSTEMS: [&str; 2] = ["devfs", "devicefs"];
const HIDDEN_MOUNT_PREFIXES: [&str; 2] = ["/System/Volumes/", "/private/var/"];
const MNT_DONTBROWSE: u32 = 0x0010_0000;
const DISK_IMAGE_MODEL: &str = "Disk Image";

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Volume {
    pub mount_point: String,
    pub filesystem: String,
    pub is_network: bool,
    pub total_bytes: u64,
    pub available_bytes: u64,
}

pub fn is_network_filesystem(name: &str) -> bool {
    NETWORK_FILESYSTEMS.contains(&name)
}

/// System-internal mounts add noise. The Data volume is what `/` already reports.
pub fn is_user_visible(mount_point: &str, filesystem: &str, flags: u32, total_bytes: u64) -> bool {
    total_bytes > 0
        && flags & MNT_DONTBROWSE == 0
        && !HIDDEN_FILESYSTEMS.contains(&filesystem)
        && !HIDDEN_MOUNT_PREFIXES.iter().any(|p| mount_point.starts_with(p))
}

pub fn is_physical_disk(model: Option<&str>) -> bool {
    model != Some(DISK_IMAGE_MODEL)
}

pub fn read_volumes() -> Vec<Volume> {
    let mut mounts: *mut libc::statfs = std::ptr::null_mut();
    // SAFETY: getmntinfo points `mounts` at `count` entries it owns; copied out right away.
    let entries = unsafe {
        let count = libc::getmntinfo(&mut mounts, libc::MNT_NOWAIT);
        if count <= 0 || mounts.is_null() {
            return Vec::new();
        }
        std::slice::from_raw_parts(mounts, count as usize)
    };
    entries
        .iter()
        .filter_map(|m| {
            // SAFETY: the kernel NUL-terminates these fixed-size name fields.
            let (mount_point, filesystem) = unsafe {
                (
                    CStr::from_ptr(m.f_mntonname.as_ptr()).to_string_lossy().into_owned(),
                    CStr::from_ptr(m.f_fstypename.as_ptr()).to_string_lossy().into_owned(),
                )
            };
            let block = u64::from(m.f_bsize);
            let total_bytes = m.f_blocks.saturating_mul(block);
            is_user_visible(&mount_point, &filesystem, m.f_flags, total_bytes).then(|| Volume {
                is_network: is_network_filesystem(&filesystem),
                available_bytes: m.f_bavail.saturating_mul(block),
                mount_point,
                filesystem,
                total_bytes,
            })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DiskActivity {
    pub bsd_name: String,
    pub model: Option<String>,
    pub location: Option<String>,
    pub bytes_read: u64,
    pub bytes_written: u64,
}

pub fn read_activity() -> Vec<DiskActivity> {
    iokit::services_of_class("IOBlockStorageDriver")
        .iter()
        .filter_map(|driver| {
            let stats = driver.property("Statistics")?;
            let characteristics = |key: &str, property: &str| {
                driver
                    .search(key, iokit::SEARCH_RECURSIVE | iokit::SEARCH_PARENTS)
                    .and_then(|v| iokit::dict_string(&v, property))
            };
            let model = characteristics("Device Characteristics", "Product Name")
                .map(|s| s.trim().to_string());
            if !is_physical_disk(model.as_deref()) {
                return None;
            }
            Some(DiskActivity {
                bsd_name: as_string(&driver.search("BSD Name", iokit::SEARCH_RECURSIVE)?)?,
                model,
                location: characteristics(
                    "Protocol Characteristics",
                    "Physical Interconnect Location",
                ),
                bytes_read: dict_i64(&stats, "Bytes (Read)")? as u64,
                bytes_written: dict_i64(&stats, "Bytes (Write)")? as u64,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_network_filesystems() {
        assert!(is_network_filesystem("smbfs"));
        assert!(is_network_filesystem("nfs"));
        assert!(!is_network_filesystem("apfs"));
    }

    #[test]
    fn hides_system_mounts_and_empty_ones() {
        assert!(is_user_visible("/", "apfs", 0, 100));
        assert!(is_user_visible("/Volumes/Media", "smbfs", 0, 100));
        assert!(!is_user_visible("/System/Volumes/Data", "apfs", 0, 100));
        assert!(!is_user_visible("/dev", "devfs", 0, 100));
        assert!(!is_user_visible("/Volumes/x", "apfs", MNT_DONTBROWSE, 100));
        assert!(!is_user_visible("/Volumes/x", "apfs", 0, 0));
    }

    #[test]
    fn disk_images_are_not_physical_disks() {
        assert!(is_physical_disk(Some("APPLE SSD AP8192R")));
        assert!(is_physical_disk(None));
        assert!(!is_physical_disk(Some("Disk Image")));
    }

    #[test]
    fn root_volume_has_space_figures() {
        let volumes = read_volumes();
        let root = volumes.iter().find(|v| v.mount_point == "/").unwrap();
        assert!(root.total_bytes > 0);
        assert!(root.available_bytes <= root.total_bytes);
    }

    #[test]
    fn internal_disk_has_activity() {
        assert!(read_activity().iter().any(|d| d.bytes_read > 0));
    }
}
