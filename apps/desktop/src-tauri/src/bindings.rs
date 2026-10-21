//! Generates the TypeScript IPC contract in `packages/shared-types`.
//!
//! All exports happen in this single test so the output does not depend on
//! the order in which test binaries run (ts-rs merges types that share a file
//! only within one process). The list below is the IPC contract: every type
//! that crosses the IPC boundary must be reachable from one of these roots.

use jarvis_ai::ProviderConfig;
use jarvis_core::jarvis_agent::{AgentTask, TaskSummary};
use jarvis_core::jarvis_memory::{
    Conversation, ConversationDetail, ConversationPatch, Episode, Fact, FactInput, MemoryStatus,
    Project, ProjectInput, SearchHit, SearchQuery, TransferReport,
};
use jarvis_core::jarvis_security::{AuditEntry, AuditVerification, PendingApproval};
use jarvis_core::jarvis_voice::{VoiceState, VoiceStatus, VoiceTranscript};
use jarvis_core::{
    AgentStartRequest, AgentStatus, AppInfo, ChatStreamEvent, ChatTurnRequest, ErrorPayload,
    HealthReport, ProviderModels, ProviderTestResult, RouteResolution, Settings, ToolInfo,
    UiErrorReport,
};
use jarvis_events::SystemEvent;
use jarvis_platform::{HardwareInfo, PlatformInfo, SystemMetrics};
use jarvis_runtime::{CuratedModel, ModelDownloadRequest, RuntimeRelease, RuntimeStatus};
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
        ChatTurnRequest,
        ChatStreamEvent,
        ErrorPayload,
        UiErrorReport,
        RuntimeStatus,
        RuntimeRelease,
        CuratedModel,
        ModelDownloadRequest,
        HardwareInfo,
        Conversation,
        ConversationDetail,
        ConversationPatch,
        Fact,
        FactInput,
        Project,
        ProjectInput,
        Episode,
        SearchQuery,
        SearchHit,
        MemoryStatus,
        TransferReport,
        AgentStartRequest,
        AgentStatus,
        AgentTask,
        TaskSummary,
        ToolInfo,
        PendingApproval,
        AuditEntry,
        AuditVerification,
        VoiceStatus,
        VoiceState,
        VoiceTranscript,
    );
}
