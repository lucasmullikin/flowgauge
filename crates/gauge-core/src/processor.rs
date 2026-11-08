//! CPU load and top processes, using the sysinfo crate. Both need two samples to measure usage.

use serde::Serialize;
use sysinfo::{CpuRefreshKind, MINIMUM_CPU_UPDATE_INTERVAL, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Cpu {
    pub total_percent: f32,
    pub per_core_percent: Vec<f32>,
    pub performance_cores: Option<u32>,
    pub efficiency_cores: Option<u32>,
    pub load_average: [f64; 3],
    pub uptime_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TopProcess {
    pub pid: u32,
    pub name: String,
    pub cpu_percent: f32,
    pub memory_bytes: u64,
}

fn perflevel_cores(level: u8) -> Option<u32> {
    let name = std::ffi::CString::new(format!("hw.perflevel{level}.logicalcpu")).ok()?;
    let mut value = 0u32;
    let mut size = std::mem::size_of::<u32>();
    // SAFETY: value has `size` writable bytes.
    let code = unsafe {
        libc::sysctlbyname(name.as_ptr(), (&raw mut value).cast(), &mut size, std::ptr::null_mut(), 0)
    };
    (code == 0).then_some(value)
}

pub fn top_by<K: Ord>(mut processes: Vec<TopProcess>, count: usize, key: impl Fn(&TopProcess) -> K) -> Vec<TopProcess> {
    processes.sort_by_key(|p| std::cmp::Reverse(key(p)));
    processes.truncate(count);
    processes
}

pub struct Sampler {
    system: System,
}

impl Sampler {
    pub fn new() -> Sampler {
        let system = System::new_with_specifics(RefreshKind::nothing().with_cpu(CpuRefreshKind::everything()));
        Sampler { system }
    }

    /// For repeated polling: usage is measured since the previous call, so this never blocks.
    /// Scanning every process is costly, so callers ask for it only when it's being shown.
    pub fn poll(&mut self, with_processes: bool) -> (Cpu, Option<Vec<TopProcess>>) {
        self.system.refresh_cpu_usage();
        if with_processes {
            let refresh = ProcessRefreshKind::nothing().with_cpu().with_memory();
            self.system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh);
        }
        (self.cpu(), with_processes.then(|| self.processes()))
    }

    fn cpu(&self) -> Cpu {
        let load = System::load_average();
        Cpu {
            total_percent: self.system.global_cpu_usage(),
            per_core_percent: self.system.cpus().iter().map(|c| c.cpu_usage()).collect(),
            performance_cores: perflevel_cores(0),
            efficiency_cores: perflevel_cores(1),
            load_average: [load.one, load.five, load.fifteen],
            uptime_seconds: System::uptime(),
        }
    }

    fn processes(&self) -> Vec<TopProcess> {
        self.system
            .processes()
            .values()
            .map(|p| TopProcess {
                pid: p.pid().as_u32(),
                name: p.name().to_string_lossy().into_owned(),
                cpu_percent: p.cpu_usage(),
                memory_bytes: p.memory(),
            })
            .collect()
    }

    /// Blocks long enough to get a valid usage measurement.
    pub fn sample(&mut self) -> (Cpu, Vec<TopProcess>) {
        let refresh = ProcessRefreshKind::nothing().with_cpu().with_memory();
        self.system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh);
        std::thread::sleep(MINIMUM_CPU_UPDATE_INTERVAL.max(std::time::Duration::from_millis(500)));
        self.system.refresh_cpu_usage();
        self.system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh);
        (self.cpu(), self.processes())
    }
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: u32, memory_bytes: u64) -> TopProcess {
        TopProcess { pid, name: pid.to_string(), cpu_percent: 0.0, memory_bytes }
    }

    #[test]
    fn top_by_keeps_the_largest_in_order() {
        let got = top_by(vec![process(1, 10), process(2, 30), process(3, 20)], 2, |p| p.memory_bytes);
        assert_eq!(got.iter().map(|p| p.pid).collect::<Vec<_>>(), [2, 3]);
    }

    #[test]
    fn samples_the_real_cpu() {
        let (cpu, processes) = Sampler::new().sample();
        assert!(!cpu.per_core_percent.is_empty());
        assert!(!processes.is_empty());
    }
}
