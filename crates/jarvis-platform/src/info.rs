use serde::{Deserialize, Serialize};
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};
use ts_rs::TS;

use crate::{DisplayServer, PlatformAdapter};

/// Operating system family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "platform.ts")]
pub enum OsFamily {
    Windows,
    Linux,
    Other,
}

/// Static description of the host machine, collected once at startup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "platform.ts")]
pub struct PlatformInfo {
    pub os_family: OsFamily,
    /// Human readable OS name, e.g. "Windows 11 Pro" or "Ubuntu 24.04 LTS".
    pub os_name: Option<String>,
    pub os_version: Option<String>,
    pub kernel_version: Option<String>,
    /// CPU architecture of the running binary, e.g. "x86_64".
    pub arch: String,
    pub display_server: DisplayServer,
    pub cpu_brand: Option<String>,
    pub logical_cpus: u32,
    pub physical_cores: Option<u32>,
    pub total_memory_bytes: u64,
}

impl PlatformInfo {
    /// Collects host information. Blocking; takes a few milliseconds.
    pub fn collect(adapter: &dyn PlatformAdapter) -> Self {
        let system = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(CpuRefreshKind::nothing())
                .with_memory(MemoryRefreshKind::nothing().with_ram()),
        );

        let cpu_brand = system
            .cpus()
            .first()
            .map(|cpu| cpu.brand().trim().to_owned())
            .filter(|brand| !brand.is_empty());

        Self {
            os_family: adapter.os_family(),
            os_name: System::long_os_version().or_else(System::name),
            os_version: System::os_version(),
            kernel_version: System::kernel_version(),
            arch: std::env::consts::ARCH.to_owned(),
            display_server: adapter.display_server(),
            cpu_brand,
            logical_cpus: u32::try_from(system.cpus().len()).unwrap_or(u32::MAX),
            physical_cores: System::physical_core_count().and_then(|n| u32::try_from(n).ok()),
            total_memory_bytes: system.total_memory(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::current_adapter;

    #[test]
    fn collects_plausible_host_information() {
        let adapter = current_adapter();
        let info = PlatformInfo::collect(adapter.as_ref());

        assert_eq!(info.os_family, adapter.os_family());
        assert_eq!(info.arch, std::env::consts::ARCH);
        assert!(info.logical_cpus >= 1, "at least one CPU expected");
        assert!(info.total_memory_bytes > 0, "total memory should be known");
    }

    #[test]
    fn serializes_with_camel_case_fields() {
        let info = PlatformInfo::collect(current_adapter().as_ref());
        let json = serde_json::to_value(&info).expect("serialize");
        assert!(json.get("osFamily").is_some());
        assert!(json.get("totalMemoryBytes").is_some());
    }
}
