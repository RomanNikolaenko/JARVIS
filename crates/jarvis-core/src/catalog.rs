//! Short-lived cache of the models offered by all registered providers.
//!
//! Listing models costs network round trips (Ollama needs one request per
//! model for capabilities), so the result is reused for a while. Results that
//! contain provider errors expire sooner, so a freshly started server shows up
//! quickly.

use std::sync::Arc;
use std::time::{Duration, Instant};

use jarvis_ai::{ModelInfo, ProviderDescriptor, ProviderRegistry};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Timeout for listing the models of one provider.
pub const LIST_MODELS_TIMEOUT: Duration = Duration::from_secs(8);
const FRESH_FOR: Duration = Duration::from_secs(30);
const FRESH_FOR_WITH_ERRORS: Duration = Duration::from_secs(5);

/// Models offered by one provider, or the reason they could not be listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub struct ProviderModels {
    pub provider: ProviderDescriptor,
    pub models: Vec<ModelInfo>,
    pub error: Option<String>,
}

#[derive(Debug)]
struct Snapshot {
    taken: Instant,
    providers: Arc<Vec<ProviderModels>>,
}

impl Snapshot {
    fn is_fresh(&self) -> bool {
        let ttl = if self.providers.iter().any(|p| p.error.is_some()) {
            FRESH_FOR_WITH_ERRORS
        } else {
            FRESH_FOR
        };
        self.taken.elapsed() < ttl
    }
}

#[derive(Debug, Default)]
pub(crate) struct ModelCatalog {
    /// Held while refreshing so concurrent callers share one refresh.
    snapshot: tokio::sync::Mutex<Option<Snapshot>>,
}

impl ModelCatalog {
    pub(crate) async fn get(
        &self,
        registry: &ProviderRegistry,
        refresh: bool,
    ) -> Arc<Vec<ProviderModels>> {
        let mut snapshot = self.snapshot.lock().await;
        if !refresh
            && let Some(current) = snapshot.as_ref()
            && current.is_fresh()
        {
            return Arc::clone(&current.providers);
        }
        let providers = Arc::new(fetch(registry).await);
        *snapshot = Some(Snapshot {
            taken: Instant::now(),
            providers: Arc::clone(&providers),
        });
        providers
    }

    pub(crate) async fn invalidate(&self) {
        *self.snapshot.lock().await = None;
    }
}

async fn fetch(registry: &ProviderRegistry) -> Vec<ProviderModels> {
    let lookups = registry.providers().into_iter().map(|provider| async move {
        let descriptor = provider.descriptor().clone();
        match tokio::time::timeout(LIST_MODELS_TIMEOUT, provider.list_models()).await {
            Ok(Ok(models)) => ProviderModels {
                provider: descriptor,
                models,
                error: None,
            },
            Ok(Err(err)) => ProviderModels {
                provider: descriptor,
                models: Vec::new(),
                error: Some(err.to_string()),
            },
            Err(_) => ProviderModels {
                provider: descriptor,
                models: Vec::new(),
                error: Some("listing models timed out".to_owned()),
            },
        }
    });
    futures::future::join_all(lookups).await
}
