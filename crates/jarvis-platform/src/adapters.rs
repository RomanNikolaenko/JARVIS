//! OS-specific adapters. This is the only module in the crate that selects
//! behaviour by target `cfg` (besides CPU feature detection).

use std::process::Command;

use crate::{DisplayServer, GpuInfo, OsFamily, PlatformAdapter};

/// Returns the adapter for the operating system this binary was built for.
pub fn current_adapter() -> Box<dyn PlatformAdapter> {
    #[cfg(windows)]
    {
        Box::new(windows::WindowsAdapter)
    }
    #[cfg(target_os = "linux")]
    {
        Box::new(linux::LinuxAdapter)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Box::new(UnsupportedAdapter)
    }
}

#[cfg(windows)]
mod windows {
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;

    use super::*;
    use crate::GpuVendor;

    /// `CREATE_NO_WINDOW`: console programs started by the GUI app must not
    /// open a console window.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const DISPLAY_CLASS: &str =
        r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";

    #[derive(Debug)]
    pub(super) struct WindowsAdapter;

    impl PlatformAdapter for WindowsAdapter {
        fn os_family(&self) -> OsFamily {
            OsFamily::Windows
        }

        fn display_server(&self) -> DisplayServer {
            DisplayServer::Windows
        }

        fn gpus(&self) -> Vec<GpuInfo> {
            match read_display_adapters() {
                Ok(gpus) => gpus,
                Err(err) => {
                    tracing::debug!("cannot read display adapters from the registry: {err}");
                    Vec::new()
                }
            }
        }

        fn vulkan_available(&self) -> bool {
            let system = std::env::var_os("SystemRoot")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
            system.join("System32").join("vulkan-1.dll").is_file()
        }

        fn prepare_background_command(&self, command: &mut Command) {
            command.creation_flags(CREATE_NO_WINDOW);
        }
    }

    fn read_display_adapters() -> windows_registry::Result<Vec<GpuInfo>> {
        let class = windows_registry::LOCAL_MACHINE.open(DISPLAY_CLASS)?;
        let mut gpus = Vec::new();
        for name in class.keys()? {
            if !name.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            let Ok(key) = class.open(&name) else {
                continue;
            };
            let Ok(description) = key.get_string("DriverDesc") else {
                continue;
            };
            let device_id = key
                .get_string("MatchingDeviceId")
                .unwrap_or_default()
                .to_ascii_lowercase();
            // Skip virtual adapters (Microsoft Basic Display, remote display, ...).
            let Some(vendor_id) = device_id
                .split("ven_")
                .nth(1)
                .and_then(|rest| u32::from_str_radix(rest.get(..4)?, 16).ok())
            else {
                continue;
            };
            let vram = key
                .get_u64("HardwareInformation.qwMemorySize")
                .ok()
                .or_else(|| {
                    let value = key.get_value("HardwareInformation.MemorySize").ok()?;
                    let bytes: &[u8] = value.as_ref();
                    match bytes.len() {
                        8 => bytes.try_into().ok().map(u64::from_le_bytes),
                        4 => bytes
                            .try_into()
                            .ok()
                            .map(|b| u64::from(u32::from_le_bytes(b))),
                        _ => None,
                    }
                })
                .filter(|bytes| *bytes > 0);
            gpus.push(GpuInfo {
                name: description,
                vendor: GpuVendor::from_pci_id(vendor_id),
                vram_total_bytes: vram,
                vram_free_bytes: None,
            });
        }
        Ok(gpus)
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::path::Path;

    use super::*;
    use crate::{detect_linux_display_server, read_drm_gpus};

    const VULKAN_LOADERS: &[&str] = &[
        "/usr/lib/x86_64-linux-gnu/libvulkan.so.1",
        "/usr/lib/aarch64-linux-gnu/libvulkan.so.1",
        "/usr/lib64/libvulkan.so.1",
        "/usr/lib/libvulkan.so.1",
    ];

    #[derive(Debug)]
    pub(super) struct LinuxAdapter;

    impl PlatformAdapter for LinuxAdapter {
        fn os_family(&self) -> OsFamily {
            OsFamily::Linux
        }

        fn display_server(&self) -> DisplayServer {
            detect_linux_display_server(|key| std::env::var(key).ok())
        }

        fn gpus(&self) -> Vec<GpuInfo> {
            read_drm_gpus(Path::new("/sys/class/drm"))
        }

        fn vulkan_available(&self) -> bool {
            VULKAN_LOADERS.iter().any(|path| Path::new(path).exists())
        }

        fn prepare_background_command(&self, _command: &mut Command) {}
    }
}

/// Fallback for targets without a dedicated adapter. It reports the platform
/// honestly instead of pretending to support it.
#[cfg(not(any(windows, target_os = "linux")))]
#[derive(Debug)]
struct UnsupportedAdapter;

#[cfg(not(any(windows, target_os = "linux")))]
impl PlatformAdapter for UnsupportedAdapter {
    fn os_family(&self) -> OsFamily {
        OsFamily::Other
    }

    fn display_server(&self) -> DisplayServer {
        DisplayServer::Unknown
    }

    fn gpus(&self) -> Vec<GpuInfo> {
        Vec::new()
    }

    fn vulkan_available(&self) -> bool {
        false
    }

    fn prepare_background_command(&self, _command: &mut Command) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_adapter_matches_build_target() {
        let adapter = current_adapter();
        if cfg!(windows) {
            assert_eq!(adapter.os_family(), OsFamily::Windows);
            assert_eq!(adapter.display_server(), DisplayServer::Windows);
        } else if cfg!(target_os = "linux") {
            assert_eq!(adapter.os_family(), OsFamily::Linux);
        } else {
            assert_eq!(adapter.os_family(), OsFamily::Other);
        }
    }

    #[test]
    fn gpu_listing_does_not_fail() {
        // Content depends on the machine; listing must simply not panic.
        let gpus = current_adapter().gpus();
        assert!(gpus.iter().all(|g| !g.name.is_empty()));
    }
}
