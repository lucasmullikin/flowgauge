//! GPU load from the accelerator's own statistics, no private API or root needed.

use crate::iokit::{self, as_string, dict_i64};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Gpu {
    pub model: Option<String>,
    pub core_count: Option<i64>,
    pub device_percent: Option<i64>,
    pub renderer_percent: Option<i64>,
    pub tiler_percent: Option<i64>,
    pub memory_in_use_bytes: Option<i64>,
}

pub fn read() -> Vec<Gpu> {
    iokit::services_of_class("IOAccelerator")
        .iter()
        .filter_map(|entry| {
            let stats = entry.property("PerformanceStatistics")?;
            Some(Gpu {
                model: entry.property("model").and_then(|v| as_string(&v)),
                core_count: entry.i64("gpu-core-count"),
                device_percent: dict_i64(&stats, "Device Utilization %"),
                renderer_percent: dict_i64(&stats, "Renderer Utilization %"),
                tiler_percent: dict_i64(&stats, "Tiler Utilization %"),
                memory_in_use_bytes: dict_i64(&stats, "In use system memory"),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn gpu_load_is_a_percentage() {
        for gpu in super::read() {
            assert!(gpu.device_percent.is_none_or(|p| (0..=100).contains(&p)));
        }
    }
}
