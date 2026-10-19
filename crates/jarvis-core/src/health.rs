//! Aggregated health check of the JARVIS core.

use std::future::Future;
use std::time::{Duration, Instant};

use jarvis_ai::{AiProvider, ProviderKind};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::config::ConfigSource;

/// Upper bound for a single component probe.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// Upper bound for the AI component, which contacts every provider. On
/// Windows a refused local connection alone takes about two seconds.
pub const AI_PROBE_TIMEOUT: Duration = Duration::from_secs(6);
/// Upper bound for one provider health check.
pub const PROVIDER_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub enum HealthStatus {
    Healthy,
    /// Working, but with reduced functionality.
    Degraded,
    /// Not working.
    Unhealthy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub struct ComponentHealth {
    /// Stable identifier, e.g. `config` or `ai`.
    pub id: String,
    pub label: String,
    pub status: HealthStatus,
    pub message: String,
    pub latency_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub struct HealthReport {
    /// Worst status of all components.
    pub status: HealthStatus,
    pub version: String,
    pub uptime_secs: u64,
    pub checked_at_ms: u64,
    pub components: Vec<ComponentHealth>,
}

impl HealthReport {
    pub fn aggregate(components: &[ComponentHealth]) -> HealthStatus {
        components
            .iter()
            .map(|c| c.status)
            .max()
            .unwrap_or(HealthStatus::Healthy)
    }
}

/// Runs a probe with a timeout and measures its latency.
pub(crate) async fn run_probe<F>(
    id: &str,
    label: &str,
    timeout: Duration,
    probe: F,
) -> ComponentHealth
where
    F: Future<Output = (HealthStatus, String)>,
{
    let started = Instant::now();
    let (status, message) = match tokio::time::timeout(timeout, probe).await {
        Ok(result) => result,
        Err(_) => (
            HealthStatus::Unhealthy,
            format!("health probe timed out after {}s", timeout.as_secs()),
        ),
    };
    ComponentHealth {
        id: id.to_owned(),
        label: label.to_owned(),
        status,
        message,
        latency_ms: u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX),
    }
}

pub(crate) fn config_health(source: &ConfigSource) -> (HealthStatus, String) {
    match source {
        ConfigSource::File => (HealthStatus::Healthy, "Settings loaded".to_owned()),
        ConfigSource::CreatedDefaults => {
            (HealthStatus::Healthy, "Default settings created".to_owned())
        }
        ConfigSource::Fallback { error } => (
            HealthStatus::Degraded,
            format!("Using default settings: {error}"),
        ),
    }
}

pub(crate) async fn storage_health(dir: &std::path::Path) -> (HealthStatus, String) {
    match tokio::fs::metadata(dir).await {
        Ok(meta) if meta.is_dir() => (HealthStatus::Healthy, "Data directory available".to_owned()),
        Ok(_) => (
            HealthStatus::Unhealthy,
            "Data path exists but is not a directory".to_owned(),
        ),
        Err(err) => (
            HealthStatus::Unhealthy,
            format!("Data directory unavailable: {err}"),
        ),
    }
}

/// Local runtime health. The runtime is optional, so "not installed" is
/// healthy; a failed server is degraded.
pub(crate) fn runtime_health(
    installed: bool,
    state: &jarvis_runtime::ServerState,
) -> (HealthStatus, String) {
    use jarvis_runtime::ServerPhase;
    let model = state.model_id.as_deref().unwrap_or("model");
    match state.phase {
        ServerPhase::Running => (HealthStatus::Healthy, format!("Serving {model}")),
        ServerPhase::Starting => (HealthStatus::Healthy, format!("Loading {model}")),
        ServerPhase::Stopping => (HealthStatus::Healthy, "Stopping".to_owned()),
        ServerPhase::Failed => (
            HealthStatus::Degraded,
            format!(
                "Failed: {}",
                state.error.as_deref().unwrap_or("unknown error")
            ),
        ),
        ServerPhase::Stopped if installed => (
            HealthStatus::Healthy,
            "Installed; no model running".to_owned(),
        ),
        ServerPhase::Stopped => (HealthStatus::Healthy, "Not installed (optional)".to_owned()),
    }
}

/// AI health: healthy if at least one real (non-diagnostic) provider answers
/// its health check.
pub(crate) async fn ai_health(
    providers: Vec<std::sync::Arc<dyn AiProvider>>,
) -> (HealthStatus, String) {
    if providers.is_empty() {
        return (
            HealthStatus::Unhealthy,
            "No AI providers registered".to_owned(),
        );
    }

    let checks = providers.iter().map(|provider| async move {
        let result = tokio::time::timeout(PROVIDER_CHECK_TIMEOUT, provider.health_check()).await;
        (provider.descriptor().clone(), result)
    });
    let results = futures::future::join_all(checks).await;

    let mut healthy_real = Vec::new();
    let mut failing = Vec::new();
    let mut diagnostic_only = true;
    for (descriptor, result) in results {
        let is_diagnostic = descriptor.kind == ProviderKind::Diagnostic;
        diagnostic_only &= is_diagnostic;
        match result {
            Ok(Ok(())) if !is_diagnostic => healthy_real.push(descriptor.display_name),
            Ok(Ok(())) => {}
            Ok(Err(err)) => failing.push(format!("{}: {err}", descriptor.display_name)),
            Err(_) => failing.push(format!(
                "{}: health check timed out",
                descriptor.display_name
            )),
        }
    }

    if diagnostic_only {
        (
            HealthStatus::Degraded,
            "Only the diagnostic echo provider is available; no AI model is connected".to_owned(),
        )
    } else if healthy_real.is_empty() {
        (
            HealthStatus::Degraded,
            format!("No AI provider is reachable ({})", failing.join("; ")),
        )
    } else if failing.is_empty() {
        (
            HealthStatus::Healthy,
            format!("Available: {}", healthy_real.join(", ")),
        )
    } else {
        // One working provider is enough; unreachable optional providers
        // (e.g. LM Studio not started) are reported but do not degrade.
        (
            HealthStatus::Healthy,
            format!(
                "Available: {}; unreachable: {}",
                healthy_real.join(", "),
                failing.join("; ")
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use jarvis_ai::EchoProvider;

    fn component(status: HealthStatus) -> ComponentHealth {
        ComponentHealth {
            id: "x".into(),
            label: "X".into(),
            status,
            message: String::new(),
            latency_ms: 0,
        }
    }

    #[test]
    fn aggregate_is_the_worst_status() {
        assert_eq!(HealthReport::aggregate(&[]), HealthStatus::Healthy);
        assert_eq!(
            HealthReport::aggregate(&[
                component(HealthStatus::Healthy),
                component(HealthStatus::Degraded)
            ]),
            HealthStatus::Degraded
        );
        assert_eq!(
            HealthReport::aggregate(&[
                component(HealthStatus::Unhealthy),
                component(HealthStatus::Degraded)
            ]),
            HealthStatus::Unhealthy
        );
    }

    #[test]
    fn config_fallback_is_degraded() {
        let (status, message) = config_health(&ConfigSource::Fallback {
            error: "invalid TOML".into(),
        });
        assert_eq!(status, HealthStatus::Degraded);
        assert!(message.contains("invalid TOML"));
    }

    #[tokio::test]
    async fn diagnostic_only_ai_is_degraded() {
        let (status, _) = ai_health(vec![Arc::new(EchoProvider::new())]).await;
        assert_eq!(status, HealthStatus::Degraded);

        let (status, _) = ai_health(Vec::new()).await;
        assert_eq!(status, HealthStatus::Unhealthy);
    }

    #[tokio::test(start_paused = true)]
    async fn slow_probes_time_out() {
        let health = run_probe("slow", "Slow", PROBE_TIMEOUT, async {
            tokio::time::sleep(PROBE_TIMEOUT * 2).await;
            (HealthStatus::Healthy, String::new())
        })
        .await;
        assert_eq!(health.status, HealthStatus::Unhealthy);
        assert!(health.message.contains("timed out"));
    }
}
