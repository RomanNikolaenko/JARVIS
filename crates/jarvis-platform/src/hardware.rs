//! Hardware detection for local inference: CPU features, memory, GPUs and
//! the GPU APIs llama.cpp can use.
//!
//! Detection is best effort and never fails: unknown values are `None`.
//! NVIDIA GPUs are queried through `nvidia-smi` (installed with the driver);
//! other GPUs come from the OS adapter (Windows registry, Linux sysfs).

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sysinfo::{CpuRefreshKind, Disks, MemoryRefreshKind, RefreshKind, System};
use ts_rs::TS;

use crate::PlatformAdapter;

const NVIDIA_SMI_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "platform.ts")]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Other,
}

impl GpuVendor {
    /// Maps a PCI vendor id (e.g. `0x10de`) to a vendor.
    pub fn from_pci_id(id: u32) -> Self {
        match id {
            0x10de => Self::Nvidia,
            0x1002 | 0x1022 => Self::Amd,
            0x8086 => Self::Intel,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "platform.ts")]
pub struct GpuInfo {
    pub name: String,
    pub vendor: GpuVendor,
    /// Dedicated video memory. For integrated GPUs this is the (small)
    /// reserved carve-out; they can also use shared system memory.
    pub vram_total_bytes: Option<u64>,
    pub vram_free_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "platform.ts")]
pub struct CpuFeatures {
    pub avx: bool,
    pub avx2: bool,
    pub avx512f: bool,
    pub fma: bool,
    pub neon: bool,
}

impl CpuFeatures {
    pub fn detect() -> Self {
        #[cfg(target_arch = "x86_64")]
        {
            Self {
                avx: std::arch::is_x86_feature_detected!("avx"),
                avx2: std::arch::is_x86_feature_detected!("avx2"),
                avx512f: std::arch::is_x86_feature_detected!("avx512f"),
                fma: std::arch::is_x86_feature_detected!("fma"),
                neon: false,
            }
        }
        #[cfg(target_arch = "aarch64")]
        {
            Self {
                neon: std::arch::is_aarch64_feature_detected!("neon"),
                ..Self::default()
            }
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "platform.ts")]
pub struct HardwareInfo {
    pub cpu_brand: Option<String>,
    pub physical_cores: Option<u32>,
    pub logical_cpus: u32,
    pub cpu_features: CpuFeatures,
    pub total_memory_bytes: u64,
    pub available_memory_bytes: u64,
    pub gpus: Vec<GpuInfo>,
    /// A Vulkan loader is installed (needed by the Vulkan build of llama.cpp).
    pub vulkan_available: bool,
    /// NVIDIA driver version reported by `nvidia-smi`, if present.
    pub nvidia_driver: Option<String>,
}

impl HardwareInfo {
    /// Detects hardware. Blocking; may run `nvidia-smi` (up to 3 s).
    pub fn detect(adapter: &dyn PlatformAdapter) -> Self {
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

        let nvidia = query_nvidia_smi(adapter);
        let mut gpus: Vec<GpuInfo> = nvidia.as_ref().map(|n| n.gpus.clone()).unwrap_or_default();
        for gpu in adapter.gpus() {
            // nvidia-smi has precise memory figures for NVIDIA GPUs.
            if gpu.vendor == GpuVendor::Nvidia && nvidia.is_some() {
                continue;
            }
            gpus.push(gpu);
        }

        Self {
            cpu_brand,
            physical_cores: System::physical_core_count().and_then(|n| u32::try_from(n).ok()),
            logical_cpus: u32::try_from(system.cpus().len()).unwrap_or(u32::MAX),
            cpu_features: CpuFeatures::detect(),
            total_memory_bytes: system.total_memory(),
            available_memory_bytes: system.available_memory(),
            gpus,
            vulkan_available: adapter.vulkan_available(),
            nvidia_driver: nvidia.and_then(|n| n.driver),
        }
    }
}

/// Free space on the disk that holds `path`, if it can be determined.
pub fn available_disk_space(path: &Path) -> Option<u64> {
    let canonical = std::fs::canonicalize(path).ok()?;
    // Windows returns verbatim paths (`\\?\C:\...`); mount points are `C:\`.
    let path = canonical
        .to_str()
        .and_then(|s| s.strip_prefix(r"\\?\"))
        .filter(|rest| !rest.starts_with("UNC"))
        .map_or_else(|| canonical.clone(), std::path::PathBuf::from);
    let disks = Disks::new_with_refreshed_list();
    disks
        .list()
        .iter()
        .filter(|disk| path.starts_with(disk.mount_point()))
        .max_by_key(|disk| disk.mount_point().as_os_str().len())
        .map(|disk| disk.available_space())
}

/// Currently available system memory in bytes.
pub fn available_memory() -> u64 {
    let system = System::new_with_specifics(
        RefreshKind::nothing().with_memory(MemoryRefreshKind::nothing().with_ram()),
    );
    system.available_memory()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NvidiaReport {
    gpus: Vec<GpuInfo>,
    driver: Option<String>,
}

fn query_nvidia_smi(adapter: &dyn PlatformAdapter) -> Option<NvidiaReport> {
    let mut command = Command::new("nvidia-smi");
    command
        .args([
            "--query-gpu=name,memory.total,memory.free,driver_version",
            "--format=csv,noheader,nounits",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    adapter.prepare_background_command(&mut command);
    let output = run_with_timeout(command, NVIDIA_SMI_TIMEOUT)?;
    let report = parse_nvidia_smi(&output);
    (!report.gpus.is_empty()).then_some(report)
}

/// Runs a command and returns its stdout if it succeeds within `timeout`.
fn run_with_timeout(mut command: Command, timeout: Duration) -> Option<String> {
    let mut child = command.spawn().ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if started.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
        }
    }
    let mut stdout = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut stdout).ok()?;
    Some(stdout)
}

fn parse_nvidia_smi(output: &str) -> NvidiaReport {
    let mut driver = None;
    let gpus = output
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(',').map(str::trim).collect();
            let [name, total, free, version] = fields.as_slice() else {
                return None;
            };
            if driver.is_none() && !version.is_empty() {
                driver = Some((*version).to_owned());
            }
            let mib = |value: &str| value.parse::<u64>().ok().map(|v| v * 1024 * 1024);
            Some(GpuInfo {
                name: (*name).to_owned(),
                vendor: GpuVendor::Nvidia,
                vram_total_bytes: mib(total),
                vram_free_bytes: mib(free),
            })
        })
        .collect();
    NvidiaReport { gpus, driver }
}

/// Reads GPUs from Linux sysfs (`/sys/class/drm`). Separated from the
/// adapter so it can be tested against a fixture directory.
pub fn read_drm_gpus(drm_root: &Path) -> Vec<GpuInfo> {
    let Ok(entries) = std::fs::read_dir(drm_root) else {
        return Vec::new();
    };
    let mut cards: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        // `card0` but not connectors such as `card0-DP-1`.
        .filter(|name| {
            name.strip_prefix("card")
                .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        })
        .collect();
    cards.sort();

    cards
        .into_iter()
        .filter_map(|card| {
            let device = drm_root.join(&card).join("device");
            let read = |file: &str| {
                std::fs::read_to_string(device.join(file))
                    .ok()
                    .map(|s| s.trim().to_owned())
            };
            let vendor_id = read("vendor")
                .and_then(|v| u32::from_str_radix(v.trim_start_matches("0x"), 16).ok())?;
            let uevent = read("uevent").unwrap_or_default();
            let field = |key: &str| {
                uevent
                    .lines()
                    .find_map(|l| l.strip_prefix(key).map(str::to_owned))
            };
            let driver = field("DRIVER=").unwrap_or_else(|| "unknown driver".to_owned());
            let pci = field("PCI_ID=").unwrap_or_default();
            let vendor = GpuVendor::from_pci_id(vendor_id);
            let vendor_name = match vendor {
                GpuVendor::Nvidia => "NVIDIA",
                GpuVendor::Amd => "AMD",
                GpuVendor::Intel => "Intel",
                GpuVendor::Other => "Unknown",
            };
            let total = read("mem_info_vram_total").and_then(|v| v.parse::<u64>().ok());
            let used = read("mem_info_vram_used").and_then(|v| v.parse::<u64>().ok());
            Some(GpuInfo {
                name: format!("{vendor_name} GPU ({driver}) {pci}")
                    .trim()
                    .to_owned(),
                vendor,
                vram_total_bytes: total,
                vram_free_bytes: total.zip(used).map(|(t, u)| t.saturating_sub(u)),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::current_adapter;

    #[test]
    fn parses_nvidia_smi_csv() {
        let report = parse_nvidia_smi(
            "NVIDIA GeForce RTX 4070, 12282, 11000, 571.96\nNVIDIA GeForce GTX 1650, 4096, 3900, 571.96\n",
        );
        assert_eq!(report.driver.as_deref(), Some("571.96"));
        assert_eq!(report.gpus.len(), 2);
        assert_eq!(report.gpus[0].name, "NVIDIA GeForce RTX 4070");
        assert_eq!(report.gpus[0].vram_total_bytes, Some(12_282 * 1024 * 1024));
        assert_eq!(report.gpus[1].vram_free_bytes, Some(3_900 * 1024 * 1024));
        assert!(parse_nvidia_smi("garbage\n").gpus.is_empty());
    }

    #[test]
    fn reads_gpus_from_sysfs_fixture() {
        let root = tempfile::tempdir().expect("tempdir");
        let card = root.path().join("card1").join("device");
        std::fs::create_dir_all(&card).expect("mkdir");
        std::fs::create_dir_all(root.path().join("card1-DP-1")).expect("mkdir");
        std::fs::write(card.join("vendor"), "0x1002\n").expect("vendor");
        std::fs::write(card.join("uevent"), "DRIVER=amdgpu\nPCI_ID=1002:15D8\n").expect("uevent");
        std::fs::write(card.join("mem_info_vram_total"), "2147483648\n").expect("total");
        std::fs::write(card.join("mem_info_vram_used"), "147483648\n").expect("used");

        let gpus = read_drm_gpus(root.path());
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].vendor, GpuVendor::Amd);
        assert_eq!(gpus[0].name, "AMD GPU (amdgpu) 1002:15D8");
        assert_eq!(gpus[0].vram_total_bytes, Some(2_147_483_648));
        assert_eq!(gpus[0].vram_free_bytes, Some(2_000_000_000));
        assert!(read_drm_gpus(&root.path().join("missing")).is_empty());
    }

    #[test]
    fn detects_plausible_hardware() {
        let info = HardwareInfo::detect(current_adapter().as_ref());
        assert!(info.logical_cpus >= 1);
        assert!(info.total_memory_bytes > 0);
        assert!(info.available_memory_bytes <= info.total_memory_bytes);
        if cfg!(target_arch = "x86_64") {
            // Every x86-64 CPU that can run JARVIS has at least SSE2; AVX is
            // nearly universal but not guaranteed in VMs, so only check FMA⇒AVX.
            assert!(!info.cpu_features.fma || info.cpu_features.avx);
        }
    }

    #[test]
    fn reports_disk_space_for_existing_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(available_disk_space(dir.path()).is_some_and(|space| space > 0));
        assert!(available_disk_space(&dir.path().join("missing")).is_none());
    }
}
