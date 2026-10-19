//! Cross-platform system layer.
//!
//! Business logic talks to the [`PlatformAdapter`] trait and to the
//! [`SystemMonitor`]; OS-specific behaviour lives in the private `adapters`
//! module and is selected once in [`current_adapter`]. This keeps `cfg`
//! conditions out of the rest of the code base.
//!
//! All functions in this crate are synchronous and may block briefly
//! (they read `/proc`, WMI-backed counters, etc.). Async callers should run
//! them on a blocking thread pool (e.g. `tokio::task::spawn_blocking`).

mod adapters;
mod display;
mod info;
mod monitor;

pub use adapters::current_adapter;
pub use display::{DisplayServer, detect_linux_display_server};
pub use info::{OsFamily, PlatformInfo};
pub use monitor::{SystemMetrics, SystemMonitor};

/// OS-specific behaviour that cannot be expressed through portable crates.
///
/// Implementations must never fake success: when a capability is not available
/// on the current OS or session (for example on Wayland), they report it
/// explicitly.
pub trait PlatformAdapter: Send + Sync + std::fmt::Debug {
    /// The operating system family this adapter targets.
    fn os_family(&self) -> OsFamily;

    /// The graphical session type of the current process.
    fn display_server(&self) -> DisplayServer;
}
