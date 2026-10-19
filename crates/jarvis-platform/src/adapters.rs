//! OS-specific adapters. This is the only module in the crate that uses
//! target `cfg` conditions.

use crate::{DisplayServer, OsFamily, PlatformAdapter};

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
    use super::*;

    #[derive(Debug)]
    pub(super) struct WindowsAdapter;

    impl PlatformAdapter for WindowsAdapter {
        fn os_family(&self) -> OsFamily {
            OsFamily::Windows
        }

        fn display_server(&self) -> DisplayServer {
            DisplayServer::Windows
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use crate::detect_linux_display_server;

    #[derive(Debug)]
    pub(super) struct LinuxAdapter;

    impl PlatformAdapter for LinuxAdapter {
        fn os_family(&self) -> OsFamily {
            OsFamily::Linux
        }

        fn display_server(&self) -> DisplayServer {
            detect_linux_display_server(|key| std::env::var(key).ok())
        }
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
}
