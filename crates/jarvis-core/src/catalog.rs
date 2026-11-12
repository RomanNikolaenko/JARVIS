//! Short-lived cache of the models offered by all registered providers.
//!
//! Listing models costs network round trips (Ollama needs one request per
//! model for capabilities), so the result is reused for a while. Results that
//! contain provider errors expire sooner, so a freshly started server shows up
//! quickly.

use std::sync::Arc;
use std::time::Duration;

use tokio::time::Instant;

use jarvis_ai::{ModelInfo, ProviderDescriptor, ProviderRegistry};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Timeout for listing the models of one provider.
pub const LIST_MODELS_TIMEOUT: Duration = Duration::from_secs(8);
const FRESH_FOR: Duration = Duration::from_secs(30);
const FRESH_FOR_WITH_ERRORS: Duration = Duration::from_secs(5);
/// Age up to which a chat request reuses a snapshot, errors included.
const RECENT_FOR_REQUESTS: Duration = Duration::from_secs(10 * 60);

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

    /// Like [`Self::get`], but reuses any snapshot from the last ten minutes,
    /// even one with provider errors. Chat requests only read details from
    /// it (a context window, the embedding model); a new listing would delay
    /// every message by seconds while a local server is off, because Windows
    /// takes about two seconds to report a refused connection.
    pub(crate) async fn get_recent(&self, registry: &ProviderRegistry) -> Arc<Vec<ProviderModels>> {
        {
            let snapshot = self.snapshot.lock().await;
            if let Some(current) = snapshot.as_ref()
                && current.taken.elapsed() < RECENT_FOR_REQUESTS
            {
                return Arc::clone(&current.providers);
            }
        }
        self.get(registry, false).await
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use jarvis_ai::{AiError, AiProvider, ChatRequest, ChatStream, DataLocality, ProviderKind};

    use super::*;

    #[derive(Debug)]
    struct Offline {
        descriptor: ProviderDescriptor,
        listings: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl AiProvider for Offline {
        fn descriptor(&self) -> &ProviderDescriptor {
            &self.descriptor
        }
        async fn health_check(&self) -> Result<(), AiError> {
            Err(AiError::Unavailable("off".into()))
        }
        async fn list_models(&self) -> Result<Vec<ModelInfo>, AiError> {
            self.listings.fetch_add(1, Ordering::SeqCst);
            Err(AiError::Unavailable("off".into()))
        }
        async fn stream_chat(&self, _request: ChatRequest) -> Result<ChatStream, AiError> {
            Err(AiError::Unavailable("off".into()))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn requests_reuse_a_recent_snapshot_with_errors() {
        let listings = Arc::new(AtomicUsize::new(0));
        let registry = ProviderRegistry::new();
        registry
            .register(Arc::new(Offline {
                descriptor: ProviderDescriptor {
                    id: jarvis_ai::ProviderId::new("ollama"),
                    display_name: "Ollama".into(),
                    kind: ProviderKind::LocalServer,
                    locality: DataLocality::OnDevice,
                },
                listings: Arc::clone(&listings),
            }))
            .unwrap();
        let catalog = ModelCatalog::default();

        catalog.get_recent(&registry).await;
        assert_eq!(listings.load(Ordering::SeqCst), 1);
        tokio::time::advance(Duration::from_secs(60)).await;
        catalog.get_recent(&registry).await;
        assert_eq!(listings.load(Ordering::SeqCst), 1);
        // The model list on screen still refreshes the snapshot.
        catalog.get(&registry, false).await;
        assert_eq!(listings.load(Ordering::SeqCst), 2);
        tokio::time::advance(RECENT_FOR_REQUESTS).await;
        catalog.get_recent(&registry).await;
        assert_eq!(listings.load(Ordering::SeqCst), 3);
    }
}
