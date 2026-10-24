//! Secret storage: OS keychain (Windows Credential Manager, Secret Service)
//! and in-memory mock for tests.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, RwLock};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SecretError {
    #[error("failed to access OS keychain: {0}")]
    Storage(String),
    #[error("keychain is unavailable: {0}")]
    Unavailable(String),
}

/// Service name used for JARVIS secrets in the OS vault.
pub const KEYCHAIN_SERVICE: &str = "jarvis";

/// Interface for storing, retrieving, and removing sensitive credentials.
pub trait SecretStore: Send + Sync + fmt::Debug {
    /// Retrieves a secret by key. Returns `Ok(None)` if no entry exists.
    fn get_secret(&self, key: &str) -> Result<Option<String>, SecretError>;

    /// Sets or overwrites a secret by key.
    fn set_secret(&self, key: &str, secret: &str) -> Result<(), SecretError>;

    /// Deletes a secret by key. Returns `Ok(())` even if it did not exist.
    fn delete_secret(&self, key: &str) -> Result<(), SecretError>;

    /// Checks whether a secret exists.
    fn has_secret(&self, key: &str) -> Result<bool, SecretError> {
        self.get_secret(key).map(|opt| opt.is_some())
    }
}

/// OS-backed keychain using `keyring` (Windows Credential Manager / Secret Service).
#[derive(Debug, Clone)]
pub struct KeyringSecretStore {
    service: String,
}

impl KeyringSecretStore {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn entry(&self, key: &str) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(&self.service, key).map_err(|err| SecretError::Storage(err.to_string()))
    }
}

impl Default for KeyringSecretStore {
    fn default() -> Self {
        Self::new(KEYCHAIN_SERVICE)
    }
}

impl SecretStore for KeyringSecretStore {
    fn get_secret(&self, key: &str) -> Result<Option<String>, SecretError> {
        let entry = self.entry(key)?;
        match entry.get_password() {
            Ok(pass) => Ok(Some(pass)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(SecretError::Storage(err.to_string())),
        }
    }

    fn set_secret(&self, key: &str, secret: &str) -> Result<(), SecretError> {
        let entry = self.entry(key)?;
        entry
            .set_password(secret)
            .map_err(|err| SecretError::Storage(err.to_string()))
    }

    fn delete_secret(&self, key: &str) -> Result<(), SecretError> {
        let entry = self.entry(key)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(SecretError::Storage(err.to_string())),
        }
    }
}

/// In-memory secret store for testing and headless environments.
#[derive(Default, Clone)]
pub struct InMemorySecretStore {
    secrets: Arc<RwLock<HashMap<String, String>>>,
}

impl fmt::Debug for InMemorySecretStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InMemorySecretStore")
            .finish_non_exhaustive()
    }
}

impl InMemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for InMemorySecretStore {
    fn get_secret(&self, key: &str) -> Result<Option<String>, SecretError> {
        let guard = self
            .secrets
            .read()
            .map_err(|e| SecretError::Storage(e.to_string()))?;
        Ok(guard.get(key).cloned())
    }

    fn set_secret(&self, key: &str, secret: &str) -> Result<(), SecretError> {
        let mut guard = self
            .secrets
            .write()
            .map_err(|e| SecretError::Storage(e.to_string()))?;
        guard.insert(key.to_owned(), secret.to_owned());
        Ok(())
    }

    fn delete_secret(&self, key: &str) -> Result<(), SecretError> {
        let mut guard = self
            .secrets
            .write()
            .map_err(|e| SecretError::Storage(e.to_string()))?;
        guard.remove(key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires an available, unlocked OS credential vault"]
    fn os_keychain_survives_reopening_entries() {
        let key = format!("test-{}", uuid::Uuid::new_v4());
        let store = KeyringSecretStore::new("jarvis-credential-test");
        store
            .set_secret(&key, "temporary-test-value")
            .expect("write test credential");
        let reopened = KeyringSecretStore::new("jarvis-credential-test");
        let result = reopened.get_secret(&key);
        store.delete_secret(&key).expect("remove test credential");
        assert_eq!(result.expect("read"), Some("temporary-test-value".into()));
        assert!(!reopened.has_secret(&key).expect("deleted"));
    }

    #[test]
    fn in_memory_secret_store_lifecycle() {
        let store = InMemorySecretStore::new();
        assert_eq!(store.get_secret("test-key").unwrap(), None);
        assert!(!store.has_secret("test-key").unwrap());

        store.set_secret("test-key", "my-secret-val").unwrap();
        assert_eq!(
            store.get_secret("test-key").unwrap(),
            Some("my-secret-val".to_string())
        );
        assert!(store.has_secret("test-key").unwrap());

        store.delete_secret("test-key").unwrap();
        assert_eq!(store.get_secret("test-key").unwrap(), None);
        assert!(!store.has_secret("test-key").unwrap());

        // Deleting non-existent key succeeds
        store.delete_secret("test-key").unwrap();
    }
}
