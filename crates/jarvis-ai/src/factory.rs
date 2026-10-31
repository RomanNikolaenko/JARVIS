use std::fmt;
use std::sync::Arc;

use crate::anthropic::AnthropicProvider;
use crate::cli::{CliKind, CliProvider};
use crate::config::classify_locality;
use crate::gemini::GeminiProvider;
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
    fn create(
        &self,
        config: &ProviderConfig,
        api_key: Option<&str>,
    ) -> Result<Arc<dyn AiProvider>, AiError>;
}

/// Creates the providers: HTTP ones share one connection pool, CLI ones
/// start the vendor's program for each request.
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
    fn create(
        &self,
        config: &ProviderConfig,
        api_key: Option<&str>,
    ) -> Result<Arc<dyn AiProvider>, AiError> {
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
            ProviderBackend::OpenAiCompatible | ProviderBackend::OpenAi => {
                let mut provider = OpenAiCompatibleProvider::new(descriptor, url, http)
                    .with_native_tool_models(config.native_tool_models.clone());
                if config.backend == ProviderBackend::OpenAi {
                    provider = provider.for_openai_api();
                }
                if let Some(key) = api_key {
                    provider = provider.with_api_key(key);
                }
                Arc::new(provider)
            }
            ProviderBackend::Anthropic => {
                let mut provider = AnthropicProvider::new(descriptor, url, http);
                if let Some(key) = api_key {
                    provider = provider.with_api_key(key);
                }
                Arc::new(provider)
            }
            ProviderBackend::Gemini => {
                let mut provider = GeminiProvider::new(descriptor, url, http);
                if let Some(key) = api_key {
                    provider = provider.with_api_key(key);
                }
                Arc::new(provider)
            }
            // The CLI keeps its own sign-in; an API key is never passed on.
            ProviderBackend::Codex => Arc::new(CliProvider::new(descriptor, CliKind::Codex)),
            ProviderBackend::ClaudeCode => {
                Arc::new(CliProvider::new(descriptor, CliKind::ClaudeCode))
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
            .create(&ProviderConfig::defaults()[0], None)
            .expect("local provider");
        assert_eq!(local.descriptor().locality, DataLocality::OnDevice);
        assert_eq!(local.descriptor().kind, ProviderKind::LocalServer);

        let cloud = factory
            .create(
                &ProviderConfig {
                    native_tool_models: vec![],
                    id: ProviderId::new("remote"),
                    backend: ProviderBackend::OpenAiCompatible,
                    display_name: " Remote ".into(),
                    base_url: "https://llm.example.com/v1".into(),
                    enabled: true,
                },
                Some("secret-key"),
            )
            .expect("cloud provider");
        assert_eq!(cloud.descriptor().kind, ProviderKind::Cloud);
        assert_eq!(cloud.descriptor().display_name, "Remote");

        let anthropic = factory
            .create(
                &ProviderConfig {
                    native_tool_models: vec![],
                    id: ProviderId::new("anthropic"),
                    backend: ProviderBackend::Anthropic,
                    display_name: "Anthropic".into(),
                    base_url: "https://api.anthropic.com/v1".into(),
                    enabled: true,
                },
                Some("sk-ant-test"),
            )
            .expect("anthropic provider");
        assert_eq!(anthropic.descriptor().kind, ProviderKind::Cloud);

        let gemini = factory
            .create(
                &ProviderConfig {
                    native_tool_models: vec![],
                    id: ProviderId::new("gemini"),
                    backend: ProviderBackend::Gemini,
                    display_name: "Gemini".into(),
                    base_url: "https://generativelanguage.googleapis.com/v1beta".into(),
                    enabled: true,
                },
                Some("gemini-test-key"),
            )
            .expect("gemini provider");
        assert_eq!(gemini.descriptor().kind, ProviderKind::Cloud);

        for backend in [ProviderBackend::Codex, ProviderBackend::ClaudeCode] {
            let cli = factory
                .create(
                    &ProviderConfig {
                        native_tool_models: vec![],
                        id: ProviderId::new("cli"),
                        backend,
                        display_name: "CLI".into(),
                        base_url: backend.default_base_url().into(),
                        enabled: true,
                    },
                    None,
                )
                .expect("cli provider");
            assert_eq!(cli.descriptor().kind, ProviderKind::Cloud);
            assert_eq!(cli.descriptor().locality, DataLocality::ThirdPartyCloud);
            assert!(!cli.supports_native_tools());
        }
    }

    #[test]
    fn rejects_invalid_configs() {
        let mut config = ProviderConfig::defaults().remove(0);
        config.base_url = "file:///etc/passwd".into();
        assert!(matches!(
            HttpProviderFactory::new().create(&config, None),
            Err(AiError::InvalidRequest(_))
        ));
    }
}
