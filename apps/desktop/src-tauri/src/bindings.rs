//! Generates the TypeScript IPC contract in `packages/shared-types`.
//!
//! All exports happen in this single test so the output does not depend on
//! the order in which test binaries run (ts-rs merges types that share a file
//! only within one process). The list below is the IPC contract: every type
//! that crosses the IPC boundary must be reachable from one of these roots.

use jarvis_ai::ProviderConfig;
use jarvis_core::{
    AppInfo, ChatStartRequest, ChatStreamEvent, ErrorPayload, HealthReport, ProviderModels,
    ProviderTestResult, RouteResolution, Settings, UiErrorReport,
};
use jarvis_events::SystemEvent;
use jarvis_platform::{PlatformInfo, SystemMetrics};
use ts_rs::{Config, TS};

macro_rules! export_all {
    ($cfg:expr, $($ty:ty),+ $(,)?) => {
        $(
            <$ty as TS>::export_all($cfg).unwrap_or_else(|err| {
                panic!("exporting {}: {err}", stringify!($ty))
            });
        )+
    };
}

#[test]
fn export_typescript_bindings() {
    let cfg = Config::from_env();
    export_all!(
        &cfg,
        AppInfo,
        HealthReport,
        PlatformInfo,
        SystemMetrics,
        Settings,
        SystemEvent,
        ProviderModels,
        RouteResolution,
        ProviderConfig,
        ProviderTestResult,
        ChatStartRequest,
        ChatStreamEvent,
        ErrorPayload,
        UiErrorReport,
    );
}
