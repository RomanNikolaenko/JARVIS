use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sysinfo::{
    CpuRefreshKind, MemoryRefreshKind, Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind,
    System,
};
use ts_rs::TS;

/// A point-in-time sample of system resource usage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "platform.ts")]
pub struct SystemMetrics {
    /// Average CPU usage across all logical CPUs, 0–100. CPU usage is computed
    /// between two samples, so the very first sample may read 0.
    pub cpu_usage_percent: f32,
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
    /// Resident memory of the JARVIS process, when available.
    pub process_memory_bytes: Option<u64>,
    /// CPU usage of the JARVIS process normalised to all logical CPUs, 0–100.
    pub process_cpu_percent: Option<f32>,
    pub uptime_secs: u64,
    /// Unix timestamp in milliseconds.
    pub sampled_at_ms: u64,
}

#[derive(Debug)]
struct MonitorState {
    system: System,
    pid: Option<Pid>,
}

/// Samples CPU and memory usage. Keeps the previous measurement internally so
/// that CPU usage can be computed as a delta between calls.
#[derive(Debug)]
pub struct SystemMonitor {
    state: Mutex<MonitorState>,
}

impl Default for SystemMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemMonitor {
    /// Creates a monitor and records the baseline measurement. Blocking.
    pub fn new() -> Self {
        let pid = sysinfo::get_current_pid()
            .inspect_err(|err| tracing::warn!("cannot determine own process id: {err}"))
            .ok();
        let mut system = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(CpuRefreshKind::nothing().with_cpu_usage())
                .with_memory(MemoryRefreshKind::nothing().with_ram()),
        );
        if let Some(pid) = pid {
            system.refresh_processes_specifics(
                ProcessesToUpdate::Some(&[pid]),
                true,
                ProcessRefreshKind::nothing().with_cpu().with_memory(),
            );
        }
        Self {
            state: Mutex::new(MonitorState { system, pid }),
        }
    }

    /// Takes a new sample. Blocking; call from a blocking thread pool in async
    /// contexts.
    pub fn sample(&self) -> SystemMetrics {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let MonitorState { system, pid } = &mut *state;

        system.refresh_cpu_usage();
        system.refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());

        if let Some(pid) = *pid {
            system.refresh_processes_specifics(
                ProcessesToUpdate::Some(&[pid]),
                true,
                ProcessRefreshKind::nothing().with_cpu().with_memory(),
            );
        }
        let logical_cpus = system.cpus().len().max(1) as f32;
        let process = pid.and_then(|pid| system.process(pid));

        SystemMetrics {
            cpu_usage_percent: clamp_percent(system.global_cpu_usage()),
            memory_used_bytes: system.used_memory(),
            memory_total_bytes: system.total_memory(),
            process_memory_bytes: process.map(|p| p.memory()),
            process_cpu_percent: process.map(|p| clamp_percent(p.cpu_usage() / logical_cpus)),
            uptime_secs: System::uptime(),
            sampled_at_ms: unix_millis(),
        }
    }
}

fn clamp_percent(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 100.0)
    } else {
        0.0
    }
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_are_within_valid_ranges() {
        let monitor = SystemMonitor::new();
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        let metrics = monitor.sample();

        assert!((0.0..=100.0).contains(&metrics.cpu_usage_percent));
        assert!(metrics.memory_total_bytes > 0);
        assert!(metrics.memory_used_bytes <= metrics.memory_total_bytes);
        assert!(metrics.sampled_at_ms > 0);
        if let Some(cpu) = metrics.process_cpu_percent {
            assert!((0.0..=100.0).contains(&cpu));
        }
        assert!(metrics.process_memory_bytes.unwrap_or(1) > 0);
    }

    #[test]
    fn clamp_handles_out_of_range_values() {
        assert_eq!(clamp_percent(f32::NAN), 0.0);
        assert_eq!(clamp_percent(-5.0), 0.0);
        assert_eq!(clamp_percent(250.0), 100.0);
        assert_eq!(clamp_percent(42.5), 42.5);
    }
}
