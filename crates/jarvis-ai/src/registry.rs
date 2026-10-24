use std::sync::{Arc, PoisonError, RwLock};

use crate::{AiError, AiProvider, ProviderDescriptor, ProviderId};

/// Thread-safe registry of available AI providers. Keeps registration order,
/// which is the user's configuration order.
#[derive(Debug, Default)]
pub struct ProviderRegistry {
    providers: RwLock<Vec<Arc<dyn AiProvider>>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a provider. Fails if a provider with the same id exists.
    pub fn register(&self, provider: Arc<dyn AiProvider>) -> Result<(), AiError> {
        let mut providers = self
            .providers
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        let id = &provider.descriptor().id;
        if providers.iter().any(|p| p.descriptor().id == *id) {
            return Err(AiError::AlreadyRegistered(id.clone()));
        }
        providers.push(provider);
        Ok(())
    }

    /// Atomically replaces all providers. Requests already running keep
    /// their provider handle and are not affected.
    pub fn replace_all(&self, providers: Vec<Arc<dyn AiProvider>>) -> Result<(), AiError> {
        for (index, provider) in providers.iter().enumerate() {
            let id = &provider.descriptor().id;
            if providers[..index].iter().any(|p| p.descriptor().id == *id) {
                return Err(AiError::AlreadyRegistered(id.clone()));
            }
        }
        *self
            .providers
            .write()
            .unwrap_or_else(PoisonError::into_inner) = providers;
        Ok(())
    }

    pub fn get(&self, id: &ProviderId) -> Result<Arc<dyn AiProvider>, AiError> {
        self.providers
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|p| p.descriptor().id == *id)
            .cloned()
            .ok_or_else(|| AiError::ProviderNotFound(id.clone()))
    }

    /// All providers in registration order.
    pub fn providers(&self) -> Vec<Arc<dyn AiProvider>> {
        self.providers
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn descriptors(&self) -> Vec<ProviderDescriptor> {
        self.providers()
            .iter()
            .map(|provider| provider.descriptor().clone())
            .collect()
    }

    pub fn len(&self) -> usize {
        self.providers
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EchoProvider, HttpProviderFactory, ProviderConfig, ProviderFactory};

    #[test]
    fn registers_and_looks_up_providers() {
        let registry = ProviderRegistry::new();
        assert!(registry.is_empty());

        registry
            .register(Arc::new(EchoProvider::new()))
            .expect("register");
        assert_eq!(registry.len(), 1);

        let id = ProviderId::new(EchoProvider::ID);
        let provider = registry.get(&id).expect("registered provider");
        assert_eq!(provider.descriptor().id, id);
        assert_eq!(registry.descriptors()[0].id, id);
    }

    #[test]
    fn rejects_duplicates_and_reports_missing_providers() {
        let registry = ProviderRegistry::new();
        registry
            .register(Arc::new(EchoProvider::new()))
            .expect("first registration");

        let duplicate = registry.register(Arc::new(EchoProvider::new()));
        assert!(matches!(duplicate, Err(AiError::AlreadyRegistered(_))));

        let missing = registry.get(&ProviderId::new("missing"));
        assert!(matches!(missing, Err(AiError::ProviderNotFound(_))));
    }

    #[test]
    fn replace_all_keeps_order_and_rejects_duplicates() {
        let registry = ProviderRegistry::new();
        let factory = HttpProviderFactory::new();
        let configured: Vec<Arc<dyn AiProvider>> = ProviderConfig::defaults()
            .iter()
            .map(|c| factory.create(c, None).expect("provider"))
            .collect();
        let mut all: Vec<Arc<dyn AiProvider>> = vec![Arc::new(EchoProvider::new())];
        all.extend(configured);
        registry.replace_all(all).expect("replace");

        let ids: Vec<String> = registry
            .descriptors()
            .into_iter()
            .map(|d| d.id.to_string())
            .collect();
        assert_eq!(ids, vec![EchoProvider::ID, "ollama", "lm-studio"]);

        let duplicate = registry.replace_all(vec![
            Arc::new(EchoProvider::new()),
            Arc::new(EchoProvider::new()),
        ]);
        assert!(matches!(duplicate, Err(AiError::AlreadyRegistered(_))));
        assert_eq!(
            registry.len(),
            3,
            "a failed replace must not change the registry"
        );
    }
}
