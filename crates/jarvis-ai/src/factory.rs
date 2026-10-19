use std::fmt;
use std::sync::Arc;

use crate::config::classify_locality;
use crate::http::HttpClient;
use crate::ollama::OllamaProvider;
use crate::openai::OpenAiCompatibleProvider;
use crate::{
    AiError, AiProvider, DataLocality, ProviderBackend, ProviderConfig, ProviderDescriptor,
    ProviderKind,
};

/// Builds providers from user configuration. Injected into the core so tests
/// can replace network providers with fakes.
pub trait ProviderFactory: Send + Sync + fmt::Debug {
    fn create(&self, config: &ProviderConfig) -> Result<Arc<dyn AiProvider>, AiError>;
}

/// Creates HTTP providers that share one connection pool.
#[derive(Debug, Default)]
pub struct HttpProviderFactory {
    http: Arc<HttpClient>,
}

impl HttpProviderFactory {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ProviderFactory for HttpProviderFactory {
    fn create(&self, config: &ProviderConfig) -> Result<Arc<dyn AiProvider>, AiError> {
        let url = config.validate().map_err(AiError::InvalidRequest)?;
        let locality = classify_locality(&url);
        let descriptor = ProviderDescriptor {
            id: config.id.clone(),
            display_name: config.display_name.trim().to_owned(),
            kind: if locality == DataLocality::ThirdPartyCloud {
                ProviderKind::Cloud
            } else {
                ProviderKind::LocalServer
            },
            locality,
        };
        let http = Arc::clone(&self.http);
        Ok(match config.backend {
            ProviderBackend::Ollama => Arc::new(OllamaProvider::new(descriptor, url, http)),
            ProviderBackend::OpenAiCompatible => {
                Arc::new(OpenAiCompatibleProvider::new(descriptor, url, http))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProviderId;

    #[test]
    fn creates_providers_with_derived_privacy_metadata() {
        let factory = HttpProviderFactory::new();
        let local = factory
            .create(&ProviderConfig::defaults()[0])
            .expect("local provider");
        assert_eq!(local.descriptor().locality, DataLocality::OnDevice);
        assert_eq!(local.descriptor().kind, ProviderKind::LocalServer);

        let cloud = factory
            .create(&ProviderConfig {
                id: ProviderId::new("remote"),
                backend: ProviderBackend::OpenAiCompatible,
                display_name: " Remote ".into(),
                base_url: "https://llm.example.com/v1".into(),
                enabled: true,
            })
            .expect("cloud provider");
        assert_eq!(cloud.descriptor().kind, ProviderKind::Cloud);
        assert_eq!(cloud.descriptor().display_name, "Remote");
    }

    #[test]
    fn rejects_invalid_configs() {
        let mut config = ProviderConfig::defaults().remove(0);
        config.base_url = "file:///etc/passwd".into();
        assert!(matches!(
            HttpProviderFactory::new().create(&config),
            Err(AiError::InvalidRequest(_))
        ));
    }
}
