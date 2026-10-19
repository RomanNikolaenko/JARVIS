use std::collections::BTreeMap;
use std::sync::{Arc, PoisonError, RwLock};

use crate::{AiError, AiProvider, ProviderDescriptor, ProviderId};

/// Thread-safe registry of available AI providers, keyed by [`ProviderId`].
#[derive(Debug, Default)]
pub struct ProviderRegistry {
    providers: RwLock<BTreeMap<ProviderId, Arc<dyn AiProvider>>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a provider. Fails if a provider with the same id exists.
    pub fn register(&self, provider: Arc<dyn AiProvider>) -> Result<(), AiError> {
        let id = provider.descriptor().id.clone();
        let mut providers = self
            .providers
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        if providers.contains_key(&id) {
            return Err(AiError::AlreadyRegistered(id));
        }
        providers.insert(id, provider);
        Ok(())
    }

    pub fn get(&self, id: &ProviderId) -> Result<Arc<dyn AiProvider>, AiError> {
        self.providers
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .cloned()
            .ok_or_else(|| AiError::ProviderNotFound(id.clone()))
    }

    /// All providers, ordered by id.
    pub fn providers(&self) -> Vec<Arc<dyn AiProvider>> {
        self.providers
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .collect()
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
    use crate::EchoProvider;

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
}
